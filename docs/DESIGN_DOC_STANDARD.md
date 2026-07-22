# Design Doc Standard — the contract for MANIFOLD design documents

**Status: NORMATIVE. Applies to every design/architecture/decision doc written after
2026-07-03. Written by Fable as the baseline the corpus is hardened to; the standard
Opus and Sonnet inherit.**

The situation this standard exists for: design docs here are written by a strong model
with Peter in the room, and executed later by weaker models with nobody in the room.
The executing model is poor at judging architecture, poor at long-running tasks, prone
to cutting corners, and prone to improvising when a decision pops up. **A design doc's
job is to convert judgment into mechanics**: every decision pre-made, every gate
mechanically checkable, every remaining choice either trivially mechanical or an
explicit escalation. If executing a doc requires the executor to *decide* or
*self-assess*, the doc has failed.

Two audiences, one document: Peter (deciding, remembering why) and the executing agent
(building, phase by phase). Write for both — decisions carry rationale, phases carry
briefs.

**Companion:** this standard governs the artifact. The method that produces its
content — the audit, the alternative-killing, how to find the plausible-wrong
architecture §4 requires you to name — is [DESIGN_AUTHORING.md](DESIGN_AUTHORING.md).
Authors read both; executors only need this one.

---

## 1. Doc types

- **Design contract** — a feature/system design approved for later implementation
  (`*_DESIGN.md`, most of the corpus). Governed by ALL of this standard.
- **Working guide** — how-to-think docs (`DECOMPOSING_GENERATORS.md`,
  `GROUPING_GRAPHS.md`). Governed by §2 (skeleton where it fits) and §7 (style); no
  phase briefs.
- **Historical record** — shipped or closed work kept for archaeology. Move to
  `docs/archive/` when no active doc references it as a contract. A record never
  says "TODO".

## 2. The skeleton

Canonical section order for a design contract. Omit sections that genuinely don't
apply; never reorder. Extracted from the two model docs — read one before writing
your first: `GIG_RESILIENCE_DESIGN.md` (failure-audit shape),
`MULTI_DISPLAY_DESIGN.md` (data-model shape).

```
# <Name> — <what it is in plain words>

**Status:** APPROVED design, not built · <date> · <author-model>
**Prerequisites:** <other designs/phases that must land first, or "none">
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

<Intro: the governing insight in one paragraph. Peter's directives verbatim-quoted
 where a decision came from him — quotes are load-bearing; they stop re-litigation.>
<Companion docs: one line each, why they're related.>

## 1. Audit — what exists (verified <date>)
## 2. Decisions            (D-numbered, rationale, rejected alternatives)
## 3..n Design body        (data model, seams, architecture — committed, not sketched)
## §. Invariants & enforcement   (each invariant + the machine check that fails when it breaks)
## §. Phasing              (one brief per phase, per §5)
## §. Decided — do not reopen   (numbered, terse)
## §. Deferred             (explicitly not v1, with the trigger that would revive each)
```

Section-by-section requirements:

- **Status line** — a `**Status:**` line in the header that LEADS with a canonical
  state tag (`SHIPPED` / `IN PROGRESS` / `APPROVED` / `PROPOSED` / `SUPERSEDED`), then
  date, author-model, prerequisites. The leading tag is load-bearing: it is the single
  source of design status, and `.claude/hooks/design_status.py` generates the session
  status board by reading it, so **every design doc MUST carry one** and the tag must
  reflect the OVERALL state — a doc with P1 shipped but P2–P4 open leads with
  `IN PROGRESS`, not `SHIPPED`. When a doc ships, update the tag and move detail to a
  record; never leave a shipped doc claiming "not built". Status lives ONLY here —
  memory files and other docs point at the board, they never restate it. (A merge
  housekeeper, `.claude/hooks/design_status_check.py`, is available to flag a doc
  whose status went stale; install it as a `post-merge` git hook to run it on merge.)
- **Audit** — a table of what exists: piece / where (file:line) / state, and the
  instruction *extend, don't redesign*. Every claim about existing code is anchored
  (§3). The audit is a dated snapshot and says so.
- **Decisions** — `D1..Dn`. Each carries: the decision, the rationale, and — when an
  obvious alternative exists — the rejected alternative with the reason, stated as
  "Rejected: X, because Y". Naming the rejected path is what stops the executor
  reinventing it. Peter's words go in quotes when the decision was his call.
- **Design body** — committed signatures for load-bearing types (§4), seams specified
  precisely, interiors left free. Honest-cost paragraphs where a decision has real
  downsides ("Consequences, stated honestly:" — see MULTI_DISPLAY §6.1).
- **Invariants & enforcement (added 2026-07-09, from the structural audit)** — every
  invariant the design introduces or leans on, each paired with its enforcement: the
  named machine check that fails when the invariant is violated — a test by name, a
  negative `rg` gate, a compile-time shape (newtype, exhaustive match), a hook, or a
  `debug_assert!` on the governed path. A map doc, memory file, or CLAUDE.md rule is
  prose, not enforcement. `Enforcement: none — <reason>` is permitted but is an
  honest-cost line the reviewer may challenge, never a default. Evidence for the
  requirement (docs/STRUCTURAL_AUDIT_VERDICTS.md, 2026-07-09): 31 of 82 backlog bugs
  were violations of invariants that existed in prose only, and the corpus's own
  cleanest datapoint — the one duplication path with a regression test never
  regressed; every path without one did.
- **Decided — do not reopen** — the terse numbered recap of every settled question.
  This is the section an executor re-reads mid-task; keep it scannable.
- **Deferred** — everything consciously excluded, each with what would revive it.
  An executor finding a gap checks here before escalating: if it's listed, it's not
  a gap, it's a decision.
- **Open questions — no unlabeled forks (added 2026-07-05).** A design contract may
  not contain an undecided "or". Every open question is exactly one of three things:
  **decided** — moved into Decisions with rationale; **defaulted** — the doc names
  the default the executor takes AND the observable trigger that would make it wrong
  ("assume 48k; if the device reports otherwise, stop and escalate"), so a worker
  proceeds safely; or **blocking** — named in the entry state of every phase it
  gates, with a named decider (almost always Peter), and no such phase gets briefed
  until it's answered. The failure this kills: prose that reads as settled but hides
  an alternative — an orchestrator hits the fork mid-wave with nobody in the room
  and improvises, which is where hard-coded paths and silent stubs come from.
  Review tells: "or we could", "TBD", "open question", "?" in design-body prose.

## 3. Reality anchoring

Docs rot; the executing agent can't tell. Rules:

- **Every claim about existing code carries a `file:line` or `file (symbol)` anchor.**
  Un-anchored claims about the codebase are not allowed in an audit section.
- **Every audit section states its verification date.**
- **Claims that rest on another unbuilt design get an explicit marker:**
  `⚠ VERIFY-AT-IMPL: <what to check> — <the exact command or file to read>`.
  "Verify" always means running a command or reading a file, never recalling.
- **Anchors are re-verified at execution time, not trusted.** Each phase brief's
  entry-state check (§5) includes re-running the anchors that phase depends on. A
  moved or missing anchor is an escalation, not a guess-and-continue.

## 4. Architecture: the doc decides, the executor transcribes

The executing model must never make an architectural choice. Therefore:

- **Load-bearing types are committed, not sketched.** Exact struct/enum/trait
  signatures, the crate and module they live in, who owns them, which thread touches
  them. "Data model (sketch)" is acceptable only for types the phases don't touch.
- **Seams are specified precisely; interiors stay free.** Trait signatures, channel
  message types, crate dependency direction, ownership, thread residency: pinned in
  the doc. Function bodies, private helpers, test internals: the executor's business.
- **Every new piece names its in-repo precedent.** "Shape this like X at file:line"
  is the strongest guardrail a weak model gets. House patterns exist for nearly
  everything: mutations → `EditingService` commands; UI data → `ui_translate.rs`
  view-models; new nodes → `primitive!` + descriptor; bundled data → bundled-presets
  loader; per-venue data → venue profile; peripheral resilience → the Ableton-bridge
  template. A design introducing a piece with no named precedent must say so and say
  why.
- **The plausible-wrong architecture is forbidden by name.** Each design names the
  tempting wrong turn for ITS problem ("you will want an `Arc<Mutex>` here — no,
  snapshots"; "you will want to fuse these dispatches — no"). Generic rules don't
  fire at the moment of temptation; named ones do.

### The may / must-escalate line

The executor MAY freely choose: local naming, private function structure, test
internals, comment wording, iteration order where the doc is silent and behavior is
unaffected.

The executor MUST escalate (stop, write the conflict down, ask Peter) before: crossing
a crate boundary the doc doesn't cross · adding a dependency · adding shared state
(`Arc<Mutex>`/`Arc<RwLock>`) · adding a thread or channel · changing a public API
shape the doc doesn't specify · anything the doc contradicts · any decision that feels
like it needs judgment. **Escalation output format:** a short section in the phase
notes — what was expected, what was found, the smallest question whose answer unblocks.
Never: choose silently, do both paths, add a flag to defer the choice.

## 5. Phase briefs

Every phase in a Phasing section is a brief with these fields. A phase without a
brief is not executable — that's the definition.

- **Size rule: one phase = one session.** The executor degrades over long runs. Every
  phase ends at a committable, gate-passing state. If a phase can't fit, split it in
  the doc — at design time, not at execution time.
- **Entry state** — what must already exist, with the commands that prove it
  (including re-verifying the anchors this phase depends on).
- **Read-back (mandatory first step)** — the files/sections the executor must read,
  then *restate*: the decisions binding this phase, the forbidden moves, and what the
  entry-state checks found. Code before read-back is a protocol violation. (This is
  the §2.5-audit pattern — the one anti-shortcut mechanism proven on this codebase.)
- **Deliverables** — files, types, tests, by name.
- **Gate** — commands with expected results. Two kinds, use both:
  - *Positive:* named tests that must pass, PNG parity diffs, packet-byte captures,
    a measured number to report. For features that parse or import external data:
    at least one **held-out input** the builder did not develop against —
    fixture-overfitting (an importer shaped around the one file in the brief) is an
    observed failure mode.
  - *Negative:* `rg` patterns that must return **zero hits** — proving the old path
    is deleted, not paralleled; proving no `unwrap()` landed on a fallible path;
    proving no new `Arc<Mutex>`.
  "Works correctly" is banned as a gate. Self-reported success is not a gate result;
  gates are run by Peter, CI, or the orchestrating session (§8).
- **Acceptance demo (added 2026-07-05)** — the observable artifact proving the
  phase's behavior end-to-end: the exact command(s) that produce it, and the
  verification level it reaches (§10). Mandatory for any phase with a user-visible
  surface (UI, rendering, import, playback, export), gated at **L2 minimum** — an
  artifact a reviewer *looks at*, not a green test. "The buttons exist" is not a
  demo; the lane visibly rendering in the PNG is. **Affordance legibility
  (added 2026-07-07, from AUDIO_SENDS P2):** every clickable element must be
  visually distinguishable AS clickable in the static PNG — a gate that says
  "renders, labels resolve" passes affordance-blind output (the observed
  escape: consumer rows rendered as bare text, the only interactive rows in
  the panel with no chrome; the worker's self-report was factually accurate
  and still missed it). Orchestrator PNG review checks affordances, not just
  presence. **No PNG oracles for agents (Peter, 2026-07-22):** models are
  unreliable at reading images — no agent (lane, reviewer, orchestrator) may
  gate on judging an image. Every agent-run gate is a computed number or exit
  code (value tests vs CPU-computed expected, scripted pixel-diffs with stated
  thresholds, region-mean probes at named coordinates). PNG artifacts are still
  produced, but the reviewer who *looks* at them is Peter; L2 means "an artifact
  Peter looks at". A phase with no observable surface
  states `Demo: none — L1` explicitly. The demo is what forces the vertical path
  (model → command → UI → pixels) to be exercised at least once before landing;
  horizontal slices each passing their own gate while the seam between them never
  runs is the observed root of "built but invisible in the app". Since UI_AUTOMATION
  P1–P2 landed (2026-07-05), a phase whose surface the flow driver can reach targets
  **L3, not L2** — write a `scripts/ui-flows/` flow that drives the real input path,
  don't stop at a PNG a reviewer merely looks at.
- **Round-trip gate (added 2026-07-06, from BUG-036).** Any phase that touches
  serialized or persistent state gates on the ROUND TRIP, not just the create path:
  save → reload → verify the feature still behaves (for params/bindings: modulate
  *after* reload, not only after creation). The observed escape: the param-storage
  wave gated on freshly-created state; every reloaded project silently dropped its
  imported card params, and modulation died only on the reload path nobody drove.
  Create-path green is HALF a gate for stateful features. Corollary: a loader that
  cannot resolve data it deserialized must keep it inert-but-present or fail loudly —
  silent dropping is the forbidden move of load paths.
- **Content-thread work gate (added 2026-07-06, from BUG-035).** Any phase adding
  per-frame OR periodic/debounced work to the content thread gates with a
  `MANIFOLD_RENDER_TRACE=1` run (spike-triggered section breakdown; any frame >20ms
  fails the gate) — measured, not argued from code. A comment or doc claiming work is
  "off-thread" must enumerate what remains ON the thread (the observed escape claimed
  "all disk IO is off-thread", truthfully — the 59ms f16 conversion feeding that IO
  ran on-thread). Sibling rule: first-use of any resource on a per-frame path
  (pipeline, mesh upload, decode) is prewarmed at load/schedule time or the phase
  brief justifies the first-frame cost (BUG-037).
- **Performer-gesture line (added 2026-07-06).** Every phase brief with a
  performance-surface deliverable names ONE gesture a performer will actually try
  with it, and the gate exercises that gesture. The observed gap: rotation params
  shipped range-clamped — correct to every test, unusable with the first thing a VJ
  does (saw LFO for a full spin, BUG-039). The gesture line is the phase-scale
  version of DESIGN_AUTHORING §3's instrument test.
- **Phasing-completeness check (added 2026-07-07, from the dead-LANES escape).**
  Before a Phasing section is done, walk every affordance/behavior the design body
  COMMITS to (each §-section's "the user can X" claims) and confirm each appears in
  exactly one place: a phase's deliverable list, or the Deferred section with its
  revival trigger. An affordance in neither is invisible to execution: the observed
  escape — AUTOMATION_LANES §7 specified the param-chooser + "+" (the only way a
  first lane can be born by drawing), §10's P4 list never named it, the orchestrator
  built the list faithfully, one worker noticed and wrote "a later phase" into a
  draw-fn comment no one re-reads, and the doc's status honestly said "SHIPPED
  P1–P4" while the feature was unreachable in every un-recorded project. The status
  line tracks the phase list, so the phase list must cover the design — a worker's
  code comment is not a deferral record; the Deferred section is.
- **Forbidden moves** — the specific shortcuts THIS phase invites, named. Drawn from
  the observed failure catalog: fuse-for-parity · silent fallback / parallel old path
  kept alive · TODO-as-deferral · "temporary" flags · adapters/shims around a misfit
  call site · synthesizing code from memory instead of reading it · inventing infra
  that exists (inventory first) · widening scope to "improve" adjacent code ·
  silently dropping unresolvable data on a load path · landing a wave with a red test
  it caused (a red test is either fixed before landing or gets a BUG entry + explicit
  Peter ping — "another session owns it" is not a landing state).
- **Invariant enforcement deliverable (added 2026-07-09, from the structural audit).**
  A phase that introduces a new invariant, or first builds on one named in the doc's
  Invariants & enforcement section, lists that invariant's machine check among its
  deliverables by name. The invariant is not landed while its check doesn't exist —
  the class fix IS the check.
- **Test scope** — which tier per the CLAUDE.md scope rule (focused vs full workspace
  sweep), stated per phase so the executor doesn't decide. Calibrate it to the failure
  class the phase can actually produce (an id rename can't change pixels → no parity
  runs), and verify ONCE per phase, at the end — batch the work, don't run test cycles
  per sub-step. Clippy follows the same scoping: `-p <touched crates>` at phase gates —
  a `--workspace` clippy in a cold worktree is a second full build. The single
  full-workspace sweep (clippy + tests) runs ONCE per pass, at landing time, in the
  warm main checkout — not per phase, not in the worktree. (Peter, 2026-07-03:
  granular per-step testing is "massive overkill" — executor static analysis is
  trusted; the end-of-phase gate catches what matters. Amended 2026-07-10: the sweep
  moved from "final phase" to "at landing, in the main checkout" — same gate, same
  coverage, one warm full build instead of N cold ones.)

## 6. Seam briefs — refactors and API changes

Changing existing code is where executors get stuck: they avoid (wrap instead of
change), half-do (both APIs stay alive), or cascade (rewrite the neighborhood).
Any phase that changes an existing API includes a seam brief:

- **Old → new, written out.** The actual before/after signatures and the field/call
  mapping. "Replace X with Y" without both shapes is not a seam brief.
- **Call-site inventory, done at design time.** The rg/LSP sweep results in the doc:
  file:line list, count, sorted into *mechanical rewrite* (with one worked example
  per category) vs *needs the new pattern* (each individually specified). Plus the
  **re-derivation command** and the rule: *re-run it at execution time; if the count
  differs from the doc, stop and list the new sites before touching anything.*
- **Compiler-driven migration is the default technique.** Rename/delete the old
  symbol FIRST; the build errors are the exhaustive checklist. The executor cannot
  miss a call site or keep a parallel path, because red doesn't compile. Use unless
  the doc states why not (e.g. serialized names needing load-migration).
- **Deletion gate.** The phase ends with the old symbol gone: an `rg` negative gate
  proves it.
- **Misfit sites escalate, never adapt.** A call site that doesn't fit the new API
  cleanly means the API design has a gap. Stop, document the site, ask. Adapters,
  shims, and "keep the old path just for this one" are forbidden by name.
- **Scope fence.** Refactor exactly what the brief lists. Adjacent code that looks
  improvable goes in a notes file for Peter, untouched.

## 7. Style

- Prose that reads like `GIG_RESILIENCE_DESIGN.md`: dense, direct, no filler, no
  AI-tells. Tables for inventories and catalogs; prose for reasoning. Bold the
  load-bearing sentence of a paragraph.
- Honest costs stated in place, not hidden ("Consequences, stated honestly").
- Peter's directives quoted verbatim where they decided something.
- Rejected alternatives recorded where an executor might reinvent them.
- No section exists to look complete. A short doc that decides everything beats a
  long doc that surveys everything.

## 8. Execution protocol (how a phase is run)

Written here once so docs don't repeat it; every doc's header points here.

1. **Fresh session per phase.** Paste the phase brief; the doc is the context, not
   the chat history.
2. **Read-back first** (§5). No code before it.
3. **Pre-flight for stale docs:** docs deep in the build order carry re-derivation
   commands instead of baked inventories (their snapshots WILL be stale — a stale
   inventory trusted is worse than none). Run the pre-flight, write the fresh
   inventory into the session, proceed against that.
4. **Escalations pause the phase.** An escalated phase is not failed; it's paused
   with a written question. Resume when answered.
5. **Gates are run by Peter, CI, or the orchestrating session — never solely the
   executor**, from the doc's commands. The executor's own "all tests pass" is a
   claim, not a gate result.
6. **Commit only at gate-pass**, per the repo's commit discipline. A phase that
   can't pass its gate ends as an escalation, never as a "mostly done" commit.
7. **Reports confess (added 2026-07-05).** Every phase report carries two mandatory
   fields: `Shortcuts taken:` — every stub, hard-code, assumption, and
   approximation, or the explicit word "none" — and `Demo artifact:` — the path, or
   `none — L1` per §5. A confessed shortcut is a one-line fix; a hunted one is a
   debugging session — the field exists to make confession cheaper than concealment.
   An omitted field means the report is incomplete, not that there was nothing to
   confess.
8. **Landing runs the demo (added 2026-07-05).** Before merging to main, the
   orchestrating session runs the acceptance-demo command itself in the main
   checkout (worktrees lack the gitignored fixtures) and reads the artifact. The
   landing report states the level reached (§10), ends with a ≤2-minute
   click-script for Peter (numbered steps, expected observation per step), and
   appends one line per unclosed gap to `docs/VERIFICATION_DEBT.md`.
9. **Landing updates the doc (added 2026-07-05 — Peter's rule).** A landing that
   completes, starts, or blocks any phase of a design updates that design doc's
   **Status:** line and the affected phase markers in the same landing, before
   the push; the landing report quotes the new status line verbatim. A doc
   claiming "not built" over shipped code is how workers rebuild existing work —
   the 2026-07-05 baseline triage found ~16 such docs, including
   AUTOMATION_LANES_DESIGN still reading "Not implemented" after it shipped.
   Status truth is part of the definition of landed, not follow-up hygiene.
10. **The landing report is a committed file (added 2026-07-05).** Everything
    rules 8–9 require the landing report to carry — gate output verbatim, the
    §10 level reached, the click-script, deviations from the brief, the quoted
    status line, VD entries opened or carried — goes in
    `docs/landings/YYYY-MM-DD-<slug>.md`, committed in the same push as the
    landing. The chat message becomes a summary plus a pointer to that file.
    Rationale: the ledger cross-references landing reports (VD IDs, `Escaped:`
    lines), and until now those references pointed into chat transcripts that
    evaporate with the session — the same decay path §10's ledger was built to
    close, one level up. The click-scripts are the acute loss: VD-002's
    burn-down *is* a click-script, and every one written before this rule is
    gone. Template: `docs/landings/README.md`.

## 9. Hardening levels (for auditing existing docs)

- **Full treatment** — docs executing soon, against today's code: everything above,
  including baked call-site inventories and committed signatures.
- **Conformance treatment** — docs deeper in the build order: skeleton, anchors,
  verify-markers, decisions, phase briefs, forbidden moves — but pre-flight
  re-derivation commands INSTEAD of baked inventories, and signatures marked
  `⚠ VERIFY-AT-IMPL` where upstream phases may reshape them.

The build order that decides which is which: `docs/DESIGN_BUILD_ORDER.md`.

## 10. Verification levels, the debt ledger, and escapes (added 2026-07-05)

"Done" means *observed*. Every claim of doneness carries its level:

- **L0** — compiles, clippy clean.
- **L1** — tests green (unit / integration / headless).
- **L2** — behavior observed: the acceptance demo's artifact (PNG, packet capture,
  log trace) was produced and actually read by a reviewer.
- **L3** — scripted interaction: an automation-layer flow drives the real UI input
  path. Available since UI_AUTOMATION P1–P2 landed (2026-07-05): author a JSON flow
  under `scripts/ui-flows/` and run it with `cargo xtask ui-snap <scene> --script
  <flow.json>` — the two proving flows `scripts/ui-flows/select-and-inspect.json`
  (resolve a widget by name/text, click it, assert) and `drag-clip.json` (drag a clip
  via its surface target, assert the moved rect) are the precedent to copy.
- **L4** — human-in-app: Peter, live.

Rules:

- **Landing reports state the level reached.** "Shipped" with no level is banned;
  L1-shipped may never be announced as "working".
- **The gap between the level reached and the phase's target is verification
  debt:** one line in `docs/VERIFICATION_DEBT.md` at landing, burned down or
  consciously carried every wave. The pre-ledger failure mode this replaces:
  "unverified interactively" notes in landing reports and memory decayed silently
  into "shipped" (automation lanes, preset picker — both gaps were recorded,
  neither was acted on, both were found by Peter in the app on 2026-07-05).
- **Escape analysis:** a bug found in the app after an orchestrated landing gets an
  `Escaped:` line in its BUG_BACKLOG entry naming the wave and the stage that would
  have caught it (brief / gate / demo / held-out input / review). The
  countermeasures in this standard were designed from anecdotes; the escape ledger
  replaces anecdote with evidence about which stage actually leaks.
