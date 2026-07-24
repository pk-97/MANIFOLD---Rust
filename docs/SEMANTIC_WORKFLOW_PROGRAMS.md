# Semantic Workflow Programs — concept capture

**Status:** EXPLORATION · captured 2026-07-25 from a Peter ↔ K3 (lead) design discussion · not yet a design doc; no build authorized except where noted. Related: `docs/AGENT_ROUTING.md` (the running implementation), `.claude/orchestration/decisions.md` D-48..D-56 (R1 wave evidence), `rt-reflections-r1-handoff` memory.

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

## 4. The holes (skeptical pass, kept sharp)

1. **The deterministic part is a for-loop.** The runtime-owned portion (iterate queue, run gates, park failures) is real but small. The wave's success lives inside three opcodes the runtime cannot check: COMPILE_WAVE, BRIEF, REVIEW.
2. **The brief is the whole game.** A weak executor + complete brief works; a weak executor + 90% brief builds parallel infrastructure (documented dominant failure mode). The scarce resource is lead judgment pre-digesting the problem. The IR exposes that cost; it does not reduce it.
3. **Typed artifacts validate shape, not truth.** A schema-perfect Brief can name a nonexistent reuse target or a non-discriminating conviction test. Only a reading mind catches that.
4. **REVIEW is the only opcode whose oracle is a model — and it is load-bearing.** Everything else has a cheap external check. The entire cost structure (lead window economy, parallelism = review bandwidth) is downstream of this one fact.
5. **Benchmarking won't have the n.** A wave is 5–10 lanes; per-opcode model comparisons are inside binomial noise for months. Model selection stays anecdote-driven.
6. **Replay is an audit log, not a debugger.** Executor output is sampled; same input hash, different diff. Traceability yes, reproduction no.
7. **Model-independent but harness-dependent.** The transition table lives in Claude Code hooks + this repo's conventions. The IR would port; the machine wouldn't.
8. **The target moves.** The roster changed three times in a week; a frozen IR lags doctrine and starts lying.
9. **Generalization fails without oracles.** All hard gates exist because this is a Rust repo (clippy, tests, exit codes). Domains without a compiler have no GATE opcode. This is an architecture *for oracled work*, not a universal one.
10. **Semantic caching by hash rarely fires.** Plans are never bit-identical; similarity-based caching reintroduces a model judgment into the lookup path. (Where caching *does* fire: plan templates for repeated wave shapes — see §7.)

## 5. R1 coverage analysis — the one real measurement

Test: how much of the RT-reflections R1 wave was expressible in its queue file *before* it ran vs. decided mid-wave.

**Pre-expressed (and it all ran as written):** T1–T6 with exact anchors, gate commands, commit messages, seat assignments, pre-allocated BUG range, pre-named escalation triggers ("fails twice", "anchor moved", "new file outside named ones", "lead fills probe math if scaffold stalls"). The one real escalation (struct-layout fork) fired a pre-named trigger. Even the final probe debugging was pre-parked.

**Emergent (D-50..D-56, BUG-323/324/325):** seat rotation under quota pressure; Flash self-gating / thin middle (cost observation: dispatcher 10M tokens clerical vs Flash 1.67M actual); stand-down incident → freeze hook; Flash parse storms → three rounds of proxy patches; "lead reviews, does not author"; seat-identity fix; GLM-4.7's "pre-existing" mislabel caught in review.

**Reading:** program coverage ~100% — every *artifact-level* decision was in the transition table before the wave ran. But everything that broke broke *below* the IR's abstraction level: transport (D-54), identity (D-56), halt semantics (D-53), economics (D-51/52/55). ISA errata, not program bugs. The IR described the wave's logic completely and its work maybe 40% — most lead-window tokens went to runtime maintenance.

Caveats: n=1, and R1 was unusually well-compiled (kernel math pre-derived in the design doc). R2/R3 have thinner specs — that is where COMPILE_WAVE coverage gets tested.

## 6. The ratchet (Peter's counter) and its two edges

**Peter's point:** everything fixed in R1 is now *part of the machine* — hooks, proxy patches, charters. It no longer requires judgment. The system has a ratchet: judgment is consumed to produce determinism; each incident class dies exactly once; every wave makes the machine bigger.

True, with two edges:

1. **Hooks stop repeats, not novel problems.** R1's five incidents had five different mechanisms, zero repeats. Machinery kills classes; it does not slow the arrival of new classes. Whether novelty decays once the roster/config freezes is the open empirical question.
2. **Some judgment ratcheted into the charter, not the machine.** D-52/D-55 didn't eliminate judgment — they relocated and standardized it (lead = sole correctness read; design work → judgment-tier one-shots). Both piles are improvements; only the first grows the machine.

**The falsifiable test:** R2 runs on the R1-hardened machine with driver-ready queues and a frozen roster. Near-zero new D-entries → the judgment residue is converging. Another five-mechanism crop → the machine grows but the novel-incident tax is permanent. The decisions file is already the instrument.

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

## 9. Open questions / next steps

- **R2 as the pitch.** The machine's next run decides whether this is converging or permanently tax-paying (§6).
- **Hook migration list.** From §3's soft rows: retry-cap enforcement (count gate invocations per lane session), one-commit-then-stop (deny a second commit from executor-tier transcripts). Build deliberately, not reactively.
- **Verdict rationale field.** The IR has no representation for *why* a judgment was made; Verdict wants a mandatory one-line rationale, appended to the decisions file by the runtime, not by model goodwill.
- **Driver script.** Peter's call, per the handoff's standing note.
- **Plan-template library.** After R2, name the repeated shapes and pre-adversarial them.
- **The general claim stays parked.** Universal semantic IR between humans, models, and workflows — overshoot until oracle coverage exists outside code (§4.9).
