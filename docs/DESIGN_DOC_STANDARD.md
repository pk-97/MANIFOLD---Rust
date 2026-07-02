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
## §. Phasing              (one brief per phase, per §5)
## §. Decided — do not reopen   (numbered, terse)
## §. Deferred             (explicitly not v1, with the trigger that would revive each)
```

Section-by-section requirements:

- **Status line** — state (APPROVED / IN PROGRESS / SHIPPED / SUPERSEDED), date,
  author-model, prerequisites. When a doc ships, update the status and move detail to
  a record; never leave a shipped doc claiming "not built".
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
- **Decided — do not reopen** — the terse numbered recap of every settled question.
  This is the section an executor re-reads mid-task; keep it scannable.
- **Deferred** — everything consciously excluded, each with what would revive it.
  An executor finding a gap checks here before escalating: if it's listed, it's not
  a gap, it's a decision.

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
    a measured number to report.
  - *Negative:* `rg` patterns that must return **zero hits** — proving the old path
    is deleted, not paralleled; proving no `unwrap()` landed on a fallible path;
    proving no new `Arc<Mutex>`.
  "Works correctly" is banned as a gate. Self-reported success is not a gate result;
  gates are run by Peter or CI (§8).
- **Forbidden moves** — the specific shortcuts THIS phase invites, named. Drawn from
  the observed failure catalog: fuse-for-parity · silent fallback / parallel old path
  kept alive · TODO-as-deferral · "temporary" flags · adapters/shims around a misfit
  call site · synthesizing code from memory instead of reading it · inventing infra
  that exists (inventory first) · widening scope to "improve" adjacent code.
- **Test scope** — which tier per the CLAUDE.md scope rule (focused vs full workspace
  sweep), stated per phase so the executor doesn't decide.

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
5. **Gates are run by Peter or CI**, from the doc's commands. The executor's own
   "all tests pass" is a claim, not a gate result.
6. **Commit only at gate-pass**, per the repo's commit discipline. A phase that
   can't pass its gate ends as an escalation, never as a "mostly done" commit.

## 9. Hardening levels (for auditing existing docs)

- **Full treatment** — docs executing soon, against today's code: everything above,
  including baked call-site inventories and committed signatures.
- **Conformance treatment** — docs deeper in the build order: skeleton, anchors,
  verify-markers, decisions, phase briefs, forbidden moves — but pre-flight
  re-derivation commands INSTEAD of baked inventories, and signatures marked
  `⚠ VERIFY-AT-IMPL` where upstream phases may reshape them.

The build order that decides which is which: `docs/DESIGN_BUILD_ORDER.md`.
