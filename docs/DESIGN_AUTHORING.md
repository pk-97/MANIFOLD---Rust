# Design Authoring — how to think before the doc exists

**Status: NORMATIVE working guide (per DESIGN_DOC_STANDARD §1) · 2026-07-05 · Fable.**
**Audience: the model authoring designs with Peter in the room — Opus, after Fable.**

[DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) is the contract for the artifact: what
a finished design doc must contain so weaker models can execute it with nobody in the
room. This guide is the upstream half — the method that produces the content the
standard demands. The standard tells you a doc must name the plausible-wrong
architecture; this tells you how to find it. Read the standard first, then one model
doc end-to-end (`GIG_RESILIENCE_DESIGN.md` or `MULTI_DISPLAY_DESIGN.md`), then this.

Everything here was extracted from practice on this codebase, mostly from designs that
shipped and a few that had to be walked back. Where a move has a memory or doc behind
it, it's named — those are the receipts, and they generalize.

---

## 1. The intake — find out what is actually being asked

Peter's ask arrives as a feature: "I want audio sends", "clips should snap". The
design question underneath is always what it does **on stage**. MANIFOLD is his live
rig — the product frame is "Ableton + M4L, for live video" — so translate every ask
into instrument terms before designing anything: what does the performer do with this
live, what does it look like when it's working, and what does the show look like when
it's broken. A design that is elegant in the code and mute on stage is wrong. This
translation is also how you catch asks that are the wrong shape — sometimes the
feature Peter names is the workaround, and the instrument-level statement of the
problem points somewhere cheaper.

Then find the **binding constraint** before generating any solution. On this codebase
it is almost always one of five, so check all five explicitly:

1. **Hot path** — does anything here run per-frame? (engine tick, sync, render). If
   yes, the allocation/locking discipline decides more of the design than the feature
   does. `project_hot_paths` memory; "Hot-path discipline" in CLAUDE.md.
2. **Thread residency** — who owns the data, which thread mutates it, how does the
   other side see it? The two-thread model (content owns `Project`, UI gets
   `Arc<Project>` snapshots, commands one way, state the other) is settled; designs
   conform to it, never renegotiate it.
3. **Time model** — beats or seconds? Beats is primary; `Seconds` only at the edges
   (in_point, player time, delta, OSC, export). A design that stores seconds where
   beats belong will be wrong in a way that only shows up when the BPM changes live.
4. **Persistence** — does it serialize? Then: V1/V2 format, load-migration for old
   projects, and the canonical fixture (`Liveschool Live Show V6 LEDS.manifold`, 53
   layers / 2928 clips — never assume small) are all in scope from the first sketch.
5. **Performance surface** — is it a live control? Then param_values, MIDI/OSC
   binding, and the perform UI are part of the design, not a later phase
   (`feedback_param_values_is_performance_surface`).

Most designs are decided by which of these bind. State them in the doc's intro.

Before proposing anything, check it hasn't been settled: the decision log
(`guide_decision_log`), the target doc's own "Decided — do not reopen" and "Deferred"
sections, and the don't-re-propose memories. Re-proposing a settled thing costs trust
and session time; the check costs a minute.

Ask Peter only what the codebase cannot answer — SME calls, product judgment, taste.
Arrive with a recommendation and its price, not an option menu. Observed healthy rate
from past waves: one or two escalations per *design*, not per phase.

## 2. The audit — reality before invention

No design thought until you have inventoried what exists. Not as ritual — because the
single most expensive authoring failure observed here is designing against a remembered
codebase: proposing infrastructure that already ships under another name, or a
mechanism that contradicts how the real one works. The §2.5 audit rule for primitives
is the special case; this is the general one, and it's normative for every design
(`feedback_audit_before_proposing_primitives`, `feedback_dont_cascade_redesign`).

Method, in order:

- **Vocabulary sweep** — `rg` for the domain's words. You're looking for the names the
  codebase already uses, because your design must speak them.
- **Structure sweep** — LSP (`goToDefinition`, `findReferences`, `incomingCalls`) on
  the load-bearing symbols. Text search lies about trait dispatch; the LSP doesn't.
- **Read the nearest existing feature end-to-end.** Whatever you're designing, some
  shipped feature is its closest relative. Read it whole — the way §2.5 makes you open
  the reference preset and follow every wire. Skimming its API and inferring the rest
  is exactly the "argue from snippets" failure.
- **Ask the history.** `git log -S` on the central symbols. The shape you're tempted
  to call wrong is usually load-bearing for a reason one diff explains; knowing it
  keeps the design from re-fighting an old war.
- **Classify every finding**: *exists* / *one wire away from existing* / *genuinely
  new*. Write the classification into the doc's Audit section as you go — you are
  already writing the doc; the audit is not prep, it's §1 of the artifact.

Anchor every claim `file:line` at the moment you verify it, and date the audit.
Negative claims ("there is no X") get the search that would find X run before they're
written — absence is the most commonly botched claim in this repo's history.

Expect the design to **shrink** during the audit. That shrinkage is the audit working:
"genuinely new" is usually a short list, and the best designs here are mostly wiring.

## 3. Shaping the architecture — data first, seams precise, scope honest

Start from the data model, not the behavior. For every piece of state the design
introduces, answer four questions: **who owns it, which thread touches it, how does it
serialize, what mutates it.** On this codebase all four have house answers (content
thread; via snapshots; V1/V2 with migration; through `EditingService` commands), so a
design that has answered them is mostly finished — behavior follows the data shape.
When a design feels stuck, it's nearly always because one of these four is unanswered
or answered against the grain of the house model.

Decide **at the seam, free in the interior**. Pin what you'd be angry to get wrong at
review: trait signatures, channel message types, crate dependency direction, ownership,
thread residency, serialized names. Leave function bodies and private structure to the
executor. The standard (§4) demands this of the doc; the authoring skill is knowing
where the seam is — and the test is: *would two reasonable implementations diverge
observably here?* If yes, it's a seam; decide it. If no, deciding it is noise.

**Extend, don't redesign — and name the precedent.** Every new piece should be
"shaped like X at file:line". If you cannot find a precedent, treat that as evidence
your audit missed something, and only after re-checking believe you're first. House
patterns exist for nearly everything (the standard §4 lists them). The corollary is
the scope rule: fix at the root, sized by inventory. "Fundamental" means the design
removes the whole problem class — but scoped by what the audit found, not by ambition
(`feedback_fix_at_the_root_not_the_symptom` + `feedback_dont_cascade_redesign` are one
rule, not two: inventory decides the blast radius, then you commit to the real fix
inside it).

Two design smells that are hard rules here: no new shared state (`Arc<Mutex>` wants to
appear at every thread boundary; the answer is snapshots and commands), and no
transitional-state design — never architect for the migration period, architect the
end state and let the migration be a phase
(`feedback_dont_design_for_transitional_states`).

And for every committed choice, run the **instrument test**: what does this mean at
showtime? Latency the performer feels, failure mode mid-set, what the UI shows when
the subsystem degrades. A design carrying its stage consequences in writing is what
lets Peter approve it as the person who has to perform on it.

**The zero-new-systems test.** Before committing, count the design's new *identity,
addressing, or dispatch systems* — a new id scheme, a lookup map that mirrors an
existing one, a resolution funnel, a per-frame cache translating between two existing
systems. The expected answer is zero. If the design needs a translation layer between
two systems that already work, the correct move is to question one of the systems, not
to build the layer: the layer is where the next five bugs will live, because every
value now has two homes that must be kept in agreement by hand. The reference failure
is the scene panel's synthesized `scene.{doc}.{param}` ids + per-frame id map +
`resolve_scene_param`/`resolve_mod_target` funnels (SCENE_PANEL_EXPOSURE_CONVERGENCE
§3a, 2026-07-19) — a whole addressing universe built to avoid reusing the exposure
system that already did the job, and the home of BUG-237/249/250/260 in one week.
This test composes with §4: when your second candidate architecture deletes a
translation layer instead of adding one, that is strong evidence it is the right one.

## 4. Alternatives — generate two, price both, kill your favorite

One candidate architecture means you haven't found the seam yet. Generate at least
two **genuinely different** shapes — different seam, different owner, or different
layer, not the same shape with different names. The reliable trick when a second
shape won't come: move the boundary one layer up or down (store it on the clip vs.
the layer; decide it at edit time vs. render time; compute it in the graph vs. the
compositor). The second shape almost always lives at a different layer.

Price them honestly on four axes: implementation cost, hot-path cost, migration cost
(old projects must load), and **what it forecloses** — the option you'd be giving up.
The last one is where designs really differ; the first is where they only appear to.

Then **kill-pass your favorite before presenting it**
(`feedback_derivation_substitutes_for_observation`): state the strongest case it's
wrong, and the cheapest test that would catch it — then actually run that test if it's
runnable (a 20-line spike, a headless render, a fixture load). Derivation feels like
verification from the inside and is not. What survives becomes D-numbered decisions;
what dies becomes "Rejected: X, because Y" — written for the future executor who will
independently reinvent X at 2am, which is the entire reason rejections get recorded.

**"Reimplement-and-verify" is a drift carve-out — a match-audit is a missing seam.**
When a design's entire value is fidelity *by construction* (the harness shows the real
app; the exporter writes the real bytes; the preview is the real render; the fused graph
matches the unfused one), a phase step that says *"reimplement this part and verify it
matches the live path"* reintroduces the exact drift the design exists to kill — just at
a smaller granularity, where it hides longer. A `VERIFY-AT-IMPL: does X match the live
path?` gate is the tell that a seam is missing. The catch that works is a seam; the
catch that decays is an audit — a manual, discretionary "does it match?" read that
holds only while someone is paying attention. There are two fixes, and the smell is the
same in both; diagnose by asking *does this duplication carry information the other copy
doesn't?*

- **No → accidental duplication → extract a seam.** The second copy exists only because
  the code got written twice. Share the one function; there was nothing to verify.
  (Specimen: UI_HARNESS_UNIFICATION kept the immediate-pass assembly as a parallel
  harness copy behind a match-audit and produced two lookalikes — the editor 3-tree
  topology and BUG-097's overlay pass — in a single execution session. The fix,
  `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md`, deletes the copy.)
- **Yes → essential duplication → automate the equivalence.** Sometimes two
  implementations *are* the feature: freeze/fusion parity, an exporter's fast path vs.
  its reference path. You can't collapse them — the point is that two paths agree. The
  net is an **automated value-level equivalence test** run every build (this repo's
  `gpu_proofs` value-parity suite is the model), never a person eyeballing "looks the
  same."

Whichever case, the thing being retired is the manual match-audit. When a fidelity
design's phase gate reads "verify it matches," stop and pick the real net.

## 5. Foreseeing the plausible-wrong turn

The standard requires each design to forbid its tempting wrong architecture *by name*
(§4). This looks like clairvoyance; it's a checklist:

1. **Ask what a competent-but-hurried implementer reaches for first.** The generic,
   Stack-Overflow-shaped answer to this problem — a mutex, a flag, a wrapper, a fused
   kernel — is usually it. It's "plausible-wrong" precisely because it's the obvious
   move.
2. **Scan the observed failure catalog** (standard §5 forbidden-moves list:
   fuse-for-parity, silent fallback, parallel old path, TODO-as-deferral, temporary
   flags, adapter shims, synthesized code, invented infra, scope widening) and ask
   which of them this design specifically invites.
3. **Scan the feedback memories.** Each one is a wrong turn that actually happened
   here, and most generalize beyond their original incident.
4. **Check what bit last time.** If this design resembles a past migration or feature
   wave, its incident reports name the failure mode that recurs.

The tell that you've found the right one: **it's the thing you yourself were tempted
to do in §4 before the kill-pass.** Your own first instinct is the best predictor of
the executor's — you're a model too; use that.

## 6. Honest costs

Every real decision has a downside. Write it in place, in the doc, under the decision
it belongs to — the house phrase is "**Consequences, stated honestly:**"
(MULTI_DISPLAY §6.1 is the model). If you cannot name a decision's downside, you do
not understand the decision yet; go back to §4.

This is not politeness. The doc is the record Peter approves *as the person who
performs on the result* — a hidden cost robs his approval of meaning, and hidden costs
don't stay hidden: they get rediscovered by Peter, in the app, after an overnight
landing (that exact failure founded the orchestration-quality initiative). A cost
stated in the doc is a trade-off he accepted; the same cost discovered later is a bug
he's angry about. Same fact, different history, entirely different outcome.

## 7. Phasing and gates — designed now, not at execution time

Phasing is part of the design, not packaging. The rules that matter:

- **One phase = one session, ending committable.** Split at design time; an executor
  splitting mid-flight improvises, and improvisation is where stubs come from.
- **The vertical slice comes first.** The first phase with a user-visible surface must
  run the whole path — model → command → UI → pixels — once, however thin. Horizontal
  slices that each pass their own gate while the seam between them never executes is
  the observed root cause of "built but invisible in the app" (automation lanes,
  2026-07-05). Design the thin vertical path deliberately; it's rarely the natural
  first phase and always the right one.
- **If you can't write the gate, the phase is under-decided.** A mechanical gate
  (named tests, PNG diff, byte capture, rg-zero-hits deletion proof) falls out of a
  decided design; reaching for "works correctly" means a decision is missing —
  go back. Importers and parsers get a held-out input the builder never sees.
- **Choose the acceptance demo when you write the phase**, with its target L-level
  (standard §10). Since UI_AUTOMATION landed, anything the flow driver can reach
  targets L3 — a scripted flow, not a PNG someone promises to look at.
- **The phase list must cover the design** (standard §5, phasing-completeness
  check). Executors build the phase list, not the design body — an affordance the
  body commits to but no phase names simply never gets built, and the status line
  ("SHIPPED P1–P4") stays honest while the design ships incomplete. Walk every
  "the user can X" claim; each lands in a phase's deliverables or in Deferred
  with a trigger. The dead-LANES escape (AUTOMATION_LANES §7 chooser, 2026-07-07)
  is the proof case: the UX section's centerpiece affordance was absent from §10,
  so four faithful phases shipped an unreachable feature.
- **Never phase by family.** If the phase plan repeats the same integration once
  per item/family ("convert World, then Object, then Light, then Modifier"), the
  plan is manufacturing N copies of one mechanism — five dispatch blocks, five
  lookup twins — and the copies are where the dead-feature bugs breed (the scene
  panel's C-P1a..d split produced exactly this; one family's click path died
  unnoticed in each copy). Phase by *layer* instead: one phase builds the single
  generic host, the next flows every family through it. If the families genuinely
  differ, the differences belong in data (a table the host reads), not in five
  code paths.
- **Gates must be behavioral, and a blind oracle is a stop sign.** Deletion greps
  and unit tests prove structure, not behavior; the scene-convergence landing was
  green on both while 8 of 21 scene flow scripts were silently dead (BUG-252).
  Two rules follow. A landing that claims flow verification must account for every
  flow file on disk for that surface — a count match, not a named subset chosen
  post hoc. And when the harness *cannot observe* the behavior that matters most
  (BUG-239: live values were invisible to `--script`), the options are fix the oracle
  or don't land — waiving the most important assertion and landing anyway is how
  "green" came to mean nothing on this surface. (BUG-239 was later fixed the right
  way — WS3 2026-07-21 gave converged rows queryable names + a `ScrollTo` action, so
  a flow can now drive and observe the slider it once could only waive.)

## 8. Done deciding vs. done surveying

The finish test for a design doc: **re-read it as the executor** — a capable but
literal model, alone, at a random phase. Anywhere *you* would have to think, the doc
owes a decision, a default-with-trigger, or a named blocking escalation (the
no-unlabeled-forks rule, standard §2). When that pass finds nothing, the design is
done — stop. Surveying the territory further *feels* productive and is the main way
authoring effort is wasted; a short doc that decides everything beats a long doc that
surveys everything, and the baseline review found the corpus's real disease was
status rot and lying prose, not missing prose.

The doc is done deciding; it is never done being true. Landing rules (standard §8.9)
keep the status line honest afterward — but the author sets up that maintenance by
keeping decisions terse and scannable enough that updating them is cheap.

## 9. With Peter in the room

The authoring sessions are collaborative; the doc is what survives them. Practices
that make that work, learned the slow way (they double as CLAUDE.md's voice-memo
doctrine, applied to design):

- **Quote his directives verbatim** into the doc at the point they decide something.
  Quotes are load-bearing: they stop future sessions re-litigating, and they mark
  which decisions are his (product) vs. yours (technical).
- **Dissent once, with the reason, then defer** — and record both positions in the
  doc when the disagreement was real. The dissent carries information he's paying
  for; rolling over silently throws it away.
- **Bring recommendations, priced.** Enumerate an option you recommend against only
  when he'd otherwise reinvent it — then name it as rejected, with the reason.
- **Translate to the stage, every time.** He's an engineer and a performer; when he
  asks about the code he is also asking what it lets him do live. Don't make him do
  the translation.
- **Answer reflective questions with the concrete short answer**, not the
  territory-survey. When you don't know, say so and name the oracle that would
  settle it.

## 10. The same method at other altitudes

Design authoring is the ceremonial version of a method that applies to everything
Opus inherits — bug hunts and complex tasks run the same skeleton, cheaper:

- **Bug hunts**: intake = reproduce and *observe* before theorizing (printlns and
  logs outrank deduction; a green test is not a look). Audit = what does the code
  actually do, what did it do when it last worked (`git log -S` is a debugger).
  Then name the level the cause lives at — symptom, mechanism, or design — and fix
  at that level. Kill-pass the diagnosis before fixing: what *else* would produce
  exactly these symptoms? Gate = the repro passes and a regression test pins it.
  The honest-edges sections of the authoritative maps (CORE_ENGINE_MAP §13) are
  pre-computed hunt lenses; start there.
- **Complex tasks**: same intake (name the binding constraint first), same finish
  discipline (state the observable end condition before starting), same kill-pass
  before declaring victory (verify one level closer to the stage than where you
  changed things).
- **Emergent bugs** (the BUG-066 class — a feedback loop misbehaves while every
  component reads clean; also: drift, hysteresis, "it slowly goes wrong"): reading
  code CANNOT find these — the bug lives in the composition, not in any kernel, so
  every component-level audit returns "symmetric, correct" and every theory feels
  plausible. The method that worked, in order: (1) build a *quantified* observable
  first — a deterministic headless harness with a number (quadrant shares, drift
  rate), not a look; rerun must be seconds-to-minutes (BUG-066's is
  `tests/fluid3d_bias.rs`, ~12s/scenario). (2) Bisect by nulling contributors via
  params — inject past UI ranges from the harness; find the minimal configuration
  that still shows the bug. (3) Characterize the bug's *invariances* before
  theorizing: flip a sign (does it mirror?), scale a parameter (does it grow?),
  rotate the camera (sim-space or view-space?). Each probe is one run and kills a
  whole hypothesis family. (4) Refute by experiment, not argument — build the
  candidate fix as a probe and run it; in one session four theories that survived
  code-reading died on contact (each refutation is progress: record it WITH its
  evidence in the backlog entry so nobody re-chases). (5) Prefer fault-LOCALIZING
  probes over theory-testing probes — a stage-by-stage symmetry walk convicts the
  broken stage no matter whose theory is right. (6) Any "unchanged/identical"
  claim gets a machine diff, never an eyeball — one misread 0.1% difference cost a
  phantom compiler-bug detour. Budget expectation: this class takes a full session
  to *characterize* and may still hand off with the root open; a precise
  characterization plus refuted-hypothesis fence is a successful session, a
  plausible unproven diagnosis is a failed one.
- **Everywhere**: CLAUDE.md's oracle discipline governs every step of every altitude
  — cheapest *reliable* oracle for the question's class; observe over deduce; "I
  don't know" plus the oracle that would resolve it is a complete answer.

The compressed form, if you keep one paragraph: **reality first (audit before
invention) · data before behavior · decide at the seams · two shapes, priced, favorite
kill-passed · forbid your own first temptation by name · costs stated where Peter
decides · vertical slice first · stop when deciding is done · and translate everything
to the stage.** That is the method. The corpus of shipped designs shows it working;
the walked-back ones (`graph-compiler-initiative`, MEDIA_BACKEND P1) show which step
was skipped — every walk-back traces to a skipped audit or an unpriced alternative.
