# Semantic Workflow Programs — concept capture

**Status:** ARCHIVED 2026-07-31 — retired with the workflow runtime (verdict: docs/archive/WORKFLOW_RUNTIME_DESIGN.md status header). The durable pieces live on as the enforcement table (`.claude/hooks/enforcement-table.json`), `scripts/gate_runner.py`, and queue-file discipline. Still cited for live rules by `docs/GATE_RUNTIME_DESIGN.md`, `preToolUseBash.py` (persistent-cd rationale), and `probe-loop-guard.py` (SEMANTIC_WORKFLOW_PROGRAMS.md section 10 (DEBUG_INVESTIGATION), the debug skeleton). · Peter + k3 (lead)

---

## 1. The framing (Peter)

Agent frameworks make LLMs be two things at once: a semantic reasoning engine *and* an operating system (scheduler, router, state manager, failure recovery). That dual role is the source of the observed brittleness — identity confusion, dispatcher bugs, notification routing failures, permission classifiers hallucinating, agents re-deriving workflow state, reasoning budget spent on control flow instead of work.

**The inversion:** build a deterministic runtime around the model. The runtime owns scheduling, state, validation, permissions, retries, caching, logging, replay. The model is a semantic computation unit — an ALU for meaning:

```
f(context) -> typed semantic artifact
```

The worker can modify artifacts. It cannot modify the transition table.

Semantic types are first-class (`Plan`, `Brief`, `Diff`, `Review`, `TestReport`, `Decision`), and opcodes have schemas and postconditions:

```
IMPLEMENT : Plan -> Diff        post: compiles, tests pass, scope-respecting
TEST      : Diff -> TestReport
REVIEW    : Diff -> Review
FIX       : Review -> Diff
```

A semantic IR sits between human intent and model backends — model-independent the way LLVM is machine-independent. Humans state goals + constraints; a frontier model compiles intent into a semantic program; the runtime executes it against interchangeable backends.

**Claimed advantages:** token reduction (no control-flow deliberation), traceability, workflow optimization, semantic caching, per-opcode model benchmarking.

## 2. The wave IR — the concrete instance we already run

Drafted against the live roster (K3 lead / GLM dispatcher / DeepSeek Flash lanes). Artifacts: `Goal, WavePlan, Brief, WorktreeSlot, Diff, GateReport, Verdict, ParkedItem, Decision, Landing`. The Brief is the load-bearing type — it must be *complete* (findings, anchors, reuse target, conviction test, gates, scope) or it is malformed; completeness is the token-saving mechanism.

Opcodes:

```
COMPILE_WAVE : Goal  -> WavePlan                 [lead — judgment]
ADVERSARIAL  : WavePlan -> WavePlan              [lead fork attacks the set]
BRIEF        : Slice -> Brief                    [dispatcher, from template]
ACQUIRE      : Slice -> WorktreeSlot             [runtime — slot-ring hook]
EXECUTE      : Brief × Slot -> Diff × GateReport [Flash lane — one commit, stop]
GATE         : Diff -> exit codes                [runtime — zero model]
SCOPE_CHECK  : Diff × scope -> bool              [dispatcher — D-52: scope-only]
REVIEW       : Diff × Brief × Gates -> Verdict   [lead — the correctness read]
LAND         : Diff -> main                      [lead only]
PARK         : Slice × blocker -> ParkedItem     [never blocks the queue]
RECORD       : ruling -> Decision                [same session, before landing]
```

## 3. The enforcement table — the key analytical tool

Every transition is enforced by one of three things. This table is where the architecture stops being analogy:

| Enforcement | Examples | Status |
|---|---|---|
| **Hook** (machine, cannot be ignored) | slot ring, tier spawn guards, seat freeze, context ceiling, no bare `#[allow(dead_code)]` | hard |
| **Exit code** (runtime-checked) | clippy `-D warnings`, focused tests, gpu-proofs, byte-compares | hard |
| **Prompt** (model goodwill) | one-commit-then-stop, retry cap 2, briefs restating invariants | soft |

The soft transitions are the known failure surface. The table turns "make agents more reliable" into a finite ordered list: migrate soft transitions to hooks, one at a time.

**Machine form (2026-07-27, Peter):** `.claude/hooks/enforcement-table.json` is this table as data — every transition, its enforcement kind, its enforcing file. `gate_runner pre-wave` verifies it against reality (hook rows registered + present, exit-code rows present) and prints the prompt-row count as the open soft surface. A migration is a one-row edit the census counts; a dead enforcing file goes red at wave start.

## 4. The holes (skeptical pass, kept sharp)

1. **The deterministic part is a for-loop.** The runtime-owned portion (iterate queue, run gates, park failures) is real but small. The wave's success lives inside three opcodes the runtime cannot check: COMPILE_WAVE, BRIEF, REVIEW.
2. **The brief is the whole game.** A weak executor + complete brief works; a weak executor + 90% brief builds parallel infrastructure (documented dominant failure mode). The scarce resource is lead judgment pre-digesting the problem. The IR exposes that cost; it does not reduce it.
3. **Typed artifacts validate shape, not truth.** A schema-perfect Brief can name a nonexistent reuse target or a non-discriminating conviction test. Only a reading mind catches that.
4. **REVIEW is the only opcode whose oracle is a model — and it is load-bearing.** Everything else has a cheap external check. The entire cost structure (lead window economy, parallelism = review bandwidth) is downstream of this one fact.
5. **Benchmarking won't have the n.** A wave is 5–10 lanes; per-opcode model comparisons are inside binomial noise for months. Model selection stays anecdote-driven.
6. **Replay is an audit log, not a debugger.** Executor output is sampled; same input hash, different diff. Traceability yes, reproduction no. External confirmation (Compiled AI, arXiv 2604.05150): non-determinism survives temperature 0 — up to 15% accuracy variance across identical runs from MoE routing alone. Corollary: a lane rerun is a new sample, not a retry; one pass + one fail on the same gate means "unstable," never "flaky infra."
7. **Model-independent but harness-dependent.** The transition table lives in Claude Code hooks + this repo's conventions. The IR would port; the machine wouldn't.
8. **The target moves.** The roster changed three times in a week; a frozen IR lags doctrine and starts lying.
9. **Generalization fails without oracles.** All hard gates exist because this is a Rust repo (clippy, tests, exit codes). Domains without a compiler have no GATE opcode. This is an architecture *for oracled work*, not a universal one.
10. **Semantic caching by hash rarely fires.** Plans are never bit-identical; similarity-based caching reintroduces a model judgment into the lookup path. (Where caching *does* fire: plan templates for repeated wave shapes — see section 7.)

## 5. R1 coverage analysis — the one real measurement

Test: how much of the RT-reflections R1 wave was expressible in its queue file *before* it ran vs. decided mid-wave.

**Pre-expressed (and it all ran as written):** T1–T6 with exact anchors, gate commands, commit messages, seat assignments, pre-allocated BUG range, pre-named escalation triggers ("fails twice", "anchor moved", "new file outside named ones", "lead fills probe math if scaffold stalls"). The one real escalation (struct-layout fork) fired a pre-named trigger. Even the final probe debugging was pre-parked.

**Emergent (D-50..D-56, BUG-323 (graph-tool-render-cant-inject-string-bindings)/324/325):** seat rotation under quota pressure; Flash self-gating / thin middle (cost observation: dispatcher 10M tokens clerical vs Flash 1.67M actual); stand-down incident → freeze hook; Flash parse storms → three rounds of proxy patches; "lead reviews, does not author"; seat-identity fix; GLM-4.7's "pre-existing" mislabel caught in review.

**Reading:** program coverage ~100% — every *artifact-level* decision was in the transition table before the wave ran. But everything that broke broke *below* the IR's abstraction level: transport (D-54), identity (D-56), halt semantics (D-53), economics (D-51/52/55). ISA errata, not program bugs. The IR described the wave's logic completely and its work maybe 40% — most lead-window tokens went to runtime maintenance.

Caveats: n=1, and R1 was unusually well-compiled (kernel math pre-derived in the design doc). R2/R3 have thinner specs — that is where COMPILE_WAVE coverage gets tested.

## 6. The ratchet (Peter's counter) and its two edges

**Peter's point:** everything fixed in R1 is now *part of the machine* — hooks, proxy patches, charters. It no longer requires judgment. The system has a ratchet: judgment is consumed to produce determinism; each incident class dies exactly once; every wave makes the machine bigger.

True, with two edges:

1. **Hooks stop repeats, not novel problems.** R1's five incidents had five different mechanisms, zero repeats. Machinery kills classes; it does not slow the arrival of new classes. Whether novelty decays once the roster/config freezes is the open empirical question.
2. **Some judgment ratcheted into the charter, not the machine.** D-52/D-55 didn't eliminate judgment — they relocated and standardized it (lead = sole correctness read; design work → judgment-tier one-shots). Both piles are improvements; only the first grows the machine.

**The falsifiable test:** R2 runs on the R1-hardened machine with driver-ready queues and a frozen roster. Near-zero new D-entries → the judgment residue is converging. Another five-mechanism crop → the machine grows but the novel-incident tax is permanent. The decisions file is already the instrument, and the reading is mechanical: `gate_runner report --since <R2 open>` counts the entries — agreed 2026-07-27 that the number decides, not the impression.

## 7. Where it lands: strict workflow programs, not general compute

The narrowing (Peter, and the version worth betting on): don't build a general semantic computer. Build a **restricted** machine — fixed opcode set, linear queue, exit-code gates, escalation as the only branch. Not Turing-complete, and that is precisely why it can be reliable: finite-state machines are auditable, general programs aren't. You give up expressiveness; you buy reviewable runs and checkable steps.

The mental model: **a player piano, and the queue file is the roll.** The driver script (handoff: queue → one-shot lanes → exit codes → park on failure → page the lead on exceptions) is this, literally. **Do not build the driver without Peter** — standing note from the R1 handoff; R2/R3 queues are written driver-ready.

If R2 is *boring*, the next question is not "how do we generalize" — it is "which of our other recurring shapes deserve a roll": decomposition slices, doc sweeps, golden regens, release checklists. A small library of proven programs, not a universal compiler. This is also where semantic caching actually fires — at the plan-template level, not the artifact-hash level.

**Long-horizon shape (added 2026-07-25, Peter):** ESCALATE as a first-class opcode makes the loops decision-heavy, not just linear. The machine runs deterministically until it hits a "judgment required" state, suspends, and resumes on the lead's returned decision — for days if needed. The horizon is not bounded by any context window because state lives in files, not sessions: the queue, decisions.md, handoffs, and resume-notes are the program's *call stack*, and each step is a fresh one-shot session that reads the stack and continues. The overnight rotation/heartbeat pattern was the proto-version; R2/R3's driver-ready queues are the first intentional ones. The scaling constraint: the judgment callback's availability and decision quality set the loop's throughput, and decision quality is exactly as good as the state externalization — decisions-as-files is load-bearing, not hygiene.

## 8. "All you need is attention" — the economics

The flip that makes this feel next-gen: the agent-framework world asks *how do we make models better at deciding what to do next*. This says deciding-what-to-do-next is a **compiler problem, not a model problem** — write the program while thinking clearly, then let cheap models execute it. Every incident this month was a model making a control-flow decision it had no business making. This removes the opportunity.

Honest bound: it controls the **mechanical fraction**, not the judgment fraction. Writing the program and reviewing the diffs still costs attention. What changes is that you spend it **once, up front, batched** — instead of continuously supervising.

**Who can operate one (amended 2026-07-25, Peter's point, accepted):** not limited to domain experts. The R1 design doc and queue were *model-written* (Fable/K3) under Peter's review — COMPILE_WAVE, the one expensive opcode, already runs on a purchasable frontier model. Review, too, already runs on frontier models (K3 reviews every lane diff; D-50 removed Peter's look from the per-phase loop). The real moat is not knowledge but **reviewed accumulation**: the compiler's briefs are true (verified anchors, real reuse targets) because they stand on a corpus — design docs, decisions, memories — that was itself model-written and human-reviewed over months. A fresh frontier model on a random codebase produces schema-valid, semantically wrong programs at frontier fluency. So the architecture is a **patient-operator multiplier**: every seat except the validation seat is purchasable.

**The human's seat, precisely (Peter, 2026-07-25):** verification — *building the thing right* — is fully mechanizable (gates, review, the whole machine). Validation — *building the RIGHT thing* — is not, because "right" is defined by the person the product serves. The machine can prove a diff satisfies a brief; only the operator can look at the show and know whether the brief was worth writing. The human does not supervise the loop — the human *aims* it, at the product level, at whatever cadence the stakes demand. For a stage rig, that seat stays human-priced: the operator is the one standing in front of the audience when it is wrong.

Not novel as a concept — workflow engines and CI pipelines are old. What is new is the **operand**: queue tasks are semantic, executed by models that can be wrong in ways a script never is, with schemas, gates, and escalation built around that fact. Nobody's product does this well, because they all start from "make the agent smarter" instead of "give the agent less to decide."

## 8b. Stateless calls — the R3-era shape (Peter + Fable, 2026-07-29, endorsed as direction)

The narrowing past "lanes make one commit then stop": no agent sessions at all. Every opcode is a stateless API call — `f(context) → typed artifact` literally, one request, one response. The runtime owns every side effect: it assembles the context, applies the diff, runs the gates, writes the files. The model never touches a tool.

The shift that matters is not "no agents" — it is **moving the tool loop from inside the model session into the runtime**. A lane today drives tools and *voluntarily* stops after one commit; that is why the enforcement table has a soft row at all. A stateless call has no capabilities to misuse: one-commit-then-stop, retry caps, and scope discipline stop being goodwill and become structure. The entire prompt row of section 3 dies at once. It also closes most of hole 4.7 (harness-dependence): a runtime that assembles contexts and applies diffs ports anywhere; the hook machine doesn't.

How the opcodes fare:

- BRIEF, REVIEW, SCOPE_CHECK, RECORD, COMPILE_WAVE — already pure `context → artifact`. One call each today, no loss.
- EXECUTE — the one that looks agentic, and it decomposes: a **runtime-driven loop of one-shots**. Model proposes a diff; runtime applies it, runs the gate, feeds the errors back as the next call's context; capped at N rounds, park on cap. Agentless proved exactly this shape on SWE-bench (section 9's citation).
- The new cost is **context assembly**. A lane earns its keep by reading the repo itself and deciding what it needs; a one-shot call gets only what it is handed. Either the lead pre-selects files at compile time (brief-is-the-whole-game becomes total), or a retrieval opcode exists: `LOCATE : Brief → FileSet` — a model judgment reappearing, but bounded, typed, and inspectable instead of a session wandering. These retrieval-shaped steps are the **"special instructions"** (Peter's term): opcodes that are internally agentic but externally still one artifact out, budget-capped, oracle-checked where possible.

The split, stated plainly (settled 2026-07-30): the FRONTIER model authors programs at
compile time with full repo access; cheap models execute one-shot with only the pasted
inputs — no repo access at run time. Context drift is handled mechanically (`anchor:`
resolve in WORKFLOW_RUNTIME_DESIGN.md D10 — deterministic locate), and a wrong input list
is a park → regenerate the program with the failure as context. No mid-run file-request
mechanism; model-driven LOCATE stays deferred.

Honest bound, same shape as always: this removes the lanes' *freedom*, not their *work*. REVIEW stays a model call and stays load-bearing; exploratory work (debugging, design) still doesn't pre-decompose — ESCALATE remains the branch. Every incident to date lived in the freedom, though, which is why this is the version worth building the runtime for. Gated on the R2 readout (section 6); the driver-script standing note (do not build without Peter) covers the runtime too.

**Measured amendment (Peter + Fable, 2026-07-30, WORKFLOW_RUNTIME P3 shakedown).** The
EXECUTE decomposition above ("a runtime-driven loop of one-shots") was tested on real
work and holds only below a size floor: small, exactly-quotable edits landed one-shot
first try; a godfile MSL refactor failed six attempts (~383K tokens) on two structural
classes no feedback loop fixes — emitting exact-quote edits at that size, and correctness
that only a compiler can see — and took a lane one pass. The ruling is
WORKFLOW_RUNTIME_DESIGN.md D20 (read-vs-run step doctrine): outputs judged by reading
stay one-shot calls; outputs judged by running route to a `lane` opcode (P4, beaded) with
the same commit-then-gate contract. The runtime's deterministic skeleton — gates, probes,
parks, budget, resume — is the part that earned its keep unconditionally.

## 9. Open questions / next steps

- **R2 as the pitch.** The machine's next run decides whether this is converging or permanently tax-paying (section 6). **The operational pre-flight checklist lives in `.claude/orchestration/rt-reflections-r2-queue.md`** (blocking items + the workflow upgrades below, scoped to that wave — 2026-07-25).
- **Hook migration list.** From section 3's soft rows: retry-cap enforcement (count gate invocations per lane session), one-commit-then-stop (deny a second commit from executor-tier transcripts). Build deliberately, not reactively. Migrated 2026-07-27: gate-gaming scan + fail-streak (D9/D10 in GATE_RUNTIME_DESIGN), persistent-cd denial (section 10.5's class, promoted after a second occurrence), hook-liveness pre-wave checks.
- **Verdict rationale field.** BUILT 2026-07-27: `gate_runner review --task --verdict --subject --rationale` appends the line to decisions.md and refuses token rationales; the trail stays gate-only per GATE_RUNTIME_DESIGN D8.
- **Driver script.** Peter 2026-07-27: likely unnecessary — the lead IS the driver; the queue, hooks, and gate_runner are the mechanism. A standalone driver only ever buys unattended multi-day runs; unbuilt unless that need arrives, and still Peter's call.
- **Plan-template library.** After R2, name the repeated shapes and pre-adversarial them.
- **The general claim stays parked — but sharpened (Peter + Fable, 2026-07-27).** Universal semantic IR between humans, models, and workflows — overshoot until oracle coverage exists outside code (section 4.9). The sharpened form worth keeping: *the instruction set is the model.* A program library is a learned artifact — trained on incidents instead of gradients, one-shot per failure, no forgetting, human-reviewed updates, auditable and portable across models in a way weights aren't. Generalizing = learning program shapes across many repos and validating them against oracles; agents then compile problems into proven programs instead of improvising control flow (Agentless already proved the primitive case on SWE-bench). A training problem wearing a systems costume. R2 is the n=1 eval; still parked until it reads out.

## 10. CANDIDATE: DEBUG_INVESTIGATION — a program shape from the RT static-death hunt (2026-07-26, proposed, not adopted)

The RT static-death hunt (BUG-jddy (RT GI+reflections die when scene goes static…)) ran one full day: ~5 hours of theory-building that reading could not settle, resolved in the end by a cure-test (forced refit) that was simultaneously the stopgap and the bisect. Retrospective: every wasted hour traces to a judgment failure of a kind a fixed opcode sequence would have prevented, and every win came from a step that *was* in the discipline but executed late. A bug investigation is a recurring shape with the same skeleton — it is a plan-template candidate (section 9).

**The measured failures and their opcodes:**

1. **Keyword-vocabulary negative claim (~1h lost).** "The project has no modulator" — searched `modulator`/`lfo`; the data was under `drivers`/`waveform`. Opcode: **SCHEMA_SEARCH before any negative claim about a data file** — enumerate keys/structure; keyword search may confirm presence, never absence.
2. **Trigger overfitting (~2h lost).** First repro was transport pause; two theories built on pause; Peter corrected twice ("not JUST the pause"). Opcode: **GENERALIZE_TRIGGER after the first repro** — write the trigger's class in one sentence ("what did this repro change about the system's inputs?"), design the next test against the class. Operator reports of the symptom outside the repro's conditions are data about the class, not noise.
3. **Reading past the stall point (~2h lost).** Two hours of kernel/executor reading after the correlation "alive ⟺ refit this frame" was established. Opcode: **CURE_TEST once a perfect action-correlation exists and two read rounds have not cracked the mechanism** — force the smallest version of the correlated action (marked STOPGAP). Never wasted: works → rig safe + cause localized to the action's parts; fails → suspect eliminated. Then DECOMPOSE: test each part of the action alone; the survivor is the mechanism.
4. **Unbudgeted delegate (~200K tokens lost).** An adversarial review agent with a read-only brief wandered for an hour and ignored two stop messages. Opcode: **BUDGET every review/consult brief** — hard token/time cap, mandatory partial-report checkpoint ("report at N, incomplete is fine"), named deliverable shape; orchestrator polices at half the budget.
5. **Delayed-failure `cd` (near-miss — then the real thing).** A persistent-cwd slip nearly produced a merge into the wrong checkout an hour later. On 2026-07-27 the same mechanism fired for real: a no-op `cd` left a lead session's shell in a worktree and the landing merge silently merged a branch into itself. Second occurrence = promotion to the hook row: `preToolUseBash.py` now denies any top-level `cd` off a checkout root, in every mode. The rule's cost model is these two incidents, not tidiness.

**The skeleton the opcodes hang on:** REPRODUCE_HEADLESS_FIRST (build/borrow the instrument before reading — this one we got right, and it was the session's spine) → SCHEMA_SEARCH → REPRO → GENERALIZE_TRIGGER → SPLIT_CASE (what does NOT show the symptom — existing discipline, fired early, worked) → two read rounds max → CURE_TEST → DECOMPOSE → LAND the stopgap marked, root cause in beads.

**Honest bound (same shape as section 8's):** the program controls the mechanical fraction of debugging — the elimination sequence — not the judgment fraction. It would not have found the GPU-command-layer mechanism; nothing would have short of the evidence. What it buys is arrival at the corner in half the tokens, with the rig protected and the session's judgment budget spent on the one step that needed it. The mechanism hunt itself stays in the ESCALATE seat.

**Adoption test:** next wrong-and-not-obvious bug, run the skeleton as a checklist (no runtime needed). If it changes the order of operations even once, it has paid for the ink.
