# Payload library — reasoning moves and drift anchors

Authored by Claude Fable 5, 2026-07-03. These are the fixed texts the valves inject;
the rubric selects by id. Haiku never edits them. Edits happen in sleep passes and
are committed. Voice rule: each payload is written to arrive as a thought, not a
lecture — short, concrete, second person, zero preamble. The signature is written
for the classifier: observable transcript markers only, no mind-reading.

Cooldown classes: `standard` = 20 tool events; `slow` = 40; `once` = once per session.

2026-07-04 additions (Fable, early sleep-pass authoring with Peter): verify-claim
wrong-medium sharpening; anchor/ungrounded-resolution (specimens: 735b0dc6 +
the glTF P1c mispriced fork); coaching/price-the-fork (same P1c specimen).

2026-07-04 stopgap pair (Fable, at Peter's direction — "I NEVER want the quick
fix"): anchor/symptom-suppression (classifier) + mechanical/confessed-stopgap
(deterministic, valve-selected). Spec for the plumbing: DESIGN.md §2c.

2026-07-04 late (Fable, from Peter's Opus-vs-Fable observation):
anchor/unpackaged-deliverable — specimen: Opus delivered a Lane A worker
prompt as prose paragraphs instead of one copyable fenced block. The general
move it compiles: format output for the user's next action, not for reading.

2026-07-05 (Opus, live sleep-pass with Peter): anchor/permission-creep —
specimen: after Peter framed the live daemon as sleep-pass-in-the-moment and
told me to record the misses, I still closed a message with "want me to add it,
or leave it for the sleep pass?" on a step already sanctioned. Compiles: act on
standing authorization; don't re-ask permission for a sanctioned step. (The same
session's other gap — hedging on a conclusive observation by asking the user to
confirm an obvious test action — stays a labels.jsonl specimen, expect_family
null: its evidence often sits outside the observer's window and it's prose not a
tool call, so it's neither a clean classifier signature nor hook-catchable yet.)

2026-07-07 (Fable, final-window session with Peter): the chat-tier pair —
mechanical/ungrounded-chat-claim (zero-latency Stop-tier sibling of
anchor/ungrounded-resolution: 16 of 68 corrective fires since stop-wait v2
landed a prompt late, and text-only turns asserting repo state were the
largest class) + mechanical/landing-doc-reflex (DESIGN_DOC_STANDARD §8 rules
9–10, Peter's 2026-07-05 detection-gap flag). The queued "mispriced-fork"
candidate was found already covered — coaching/price-the-fork carries the full
pattern including the retroactive tell, and the ask-gate's semantic tier holds
"mispriced" as a deny reason — marked stale, not re-authored.

2026-07-07 late (Fable, final authoring pass — mined from the window's own
eval/observations.session.jsonl, 39 records triaged; dispositions in
PASS2_AGENDA.md §Triage): seven new moves, all UNVALIDATED, oracles in
DESIGN.md §2i — coaching/deduction-loop (glTF viewport hunt f204e253:
innocence proven four times while the bug sat on another axis),
anchor/circular-oracle (kick session 1d67eabd: "kicks are gone from the mix"
graded by the detector under debug), anchor/premature-capture (Peter's "you
didn't really discuss with me at all here", 82ee1e52), anchor/asserted-values
(bass-stems-as-THE-use-case, c9e4d45d, Peter-flagged),
coaching/explain-with-their-artifact (group-params fork: two architecture
explanations failed; walking his 4-object glb scene landed),
anchor/unheeded-warning (branch work in the main checkout past a live
shared-checkout warning, 5363065f — dormant until the T10 ledger annotation
ships), mechanical/stale-brief (design-hardening 74c8486b: both 3-day-old
queued decisions flipped on re-verification). Sharpened same pass: circling
(+2 never-fire clauses — converging fix loops, post-refactor call-site
sweeps; both graded-FP classes this window), ungrounded-resolution
(memory-file reads are not artifact provenance — the 07-07 near-miss),
git-landing payload (+its missing escape hatch: ancestor-safe scratch
branches), confessed-stopgap contract (self-disposing markers exempt,
implementation pending T2), design-primer payload (+termination-condition,
+unification-bias sentences). verify-claim deliberately untouched again —
its pass-1 rework is still unscored (one change, one measurement).
Catalog 34→41.

2026-07-08 (Opus, off-cycle single-move authoring with Peter — not a sleep
pass): coaching/state-the-property. Specimen: cf8c327c / BUG-061
(observations.session.jsonl #13) — brief said "make reset the slider's own
gesture", executor built one shared reset action but registered it per-panel
"for lower risk", leaving the skippable seam the goal existed to close. The
preventive half was homed in SESSION_PROMPT_AUTHORING.md §1 (the brief must
now state the property + the undecided forks, not just the steps; landed @
022c3bb2); this move is the REACTIVE backstop for the informal/solo case where
no §1 prompt was cut (BUG-061 itself was an informal brief). Keys on the
CONFESSED tell only (risk-minimization language at a fork) — a silently-weaker
fork pick stays invisible, the same confessed-only limit as
mechanical/confessed-stopgap. Sibling of coaching/enumerate-levels (fires on
approach-chosen, injects a reasoning move). UNVALIDATED; scoring story in
DESIGN.md §2j. Catalog 41→42.

---

## coaching/model-first
- **signature:** A new task or bug was just stated, and the first actions are edits
  or fix attempts with no prior read of the relevant mechanism and no statement of
  how the affected system works.
- **cooldown:** slow
- **payload:**
> Before changing anything: say, in two sentences, how this is supposed to work —
> what produces the behavior you're seeing. If you can't, that's the first task.
> "What edit stops the symptom" is the wrong question until "what machine makes the
> symptom" has an answer.

## coaching/invariant-frame
- **signature:** A bug investigation is underway and the searching is unfocused —
  broad greps, files opened without a stated reason, no named expectation.
- **cooldown:** slow
- **payload:**
> Frame it as a violated invariant: something that must be true here isn't. Name
> the thing that must be true. Then walk the chain that maintains it and find the
> broken link. "Which link broke" is a finite search; "what looks weird" is not.

## coaching/differential
- **signature:** The transcript says or implies this used to work — a regression,
  a "worked before X", a comparison between a passing and failing case.
- **cooldown:** slow
- **payload:**
> This is a differential problem, so don't analyze the broken case in isolation.
> Find the minimal pair: the closest working case and the smallest difference that
> flips the outcome. `git log -S` on the touched symbol, or diff the two configs.
> The difference is the suspect list.

## coaching/predict-before-look
- **signature:** Repeated reads of files or logs during an investigation with no
  stated expectation before each read — reading that produces summaries but no
  eliminations.
- **cooldown:** standard
- **payload:**
> Before the next read: say what you expect to find and what it would rule out.
> If a read can't falsify anything, it isn't evidence-gathering — it's wandering
> that feels like progress.

## coaching/discriminate
- **signature:** Two or more candidate causes were named, and the chosen next
  check doesn't distinguish between them — it's the easiest experiment, not the
  most informative one.
- **cooldown:** standard
- **payload:**
> You have multiple live hypotheses. Pick the observation that best *splits* them,
> not the one easiest to run — the check whose result differs depending on which
> hypothesis is true. One discriminating look beats three confirming ones.

## coaching/attack-the-story
- **signature:** An explanation was just asserted with confidence ("the issue is",
  "the root cause is", "this happens because") and the very next actions implement
  a fix — no step in between that tests the explanation. The assertion and the fix
  must both be the agent's own live reasoning in RECENT/LEDGER — a TASK field that
  quotes or summarizes the agent's past reasoning (e.g. Stop-hook feedback text)
  is not itself an assertion to flag.
- **cooldown:** standard
- **payload:**
> A coherent story just formed. Coherence is not correctness — the first story that
> fits the evidence is usually one of several. Ask: what fact, if true, would make
> this explanation wrong? Go check that specific fact before building on the story.

## coaching/enumerate-levels
- **signature:** A fix is being applied that mirrors the symptom's location and
  shape (guard clause at the crash site, special case at the call site) with no
  discussion of where else — or at what level — the fix could live.
- **cooldown:** standard
- **payload:**
> Before committing to this fix: name the levels it could be made at — this call
> site, the function's contract, the data's invariant, the type. The first fix that
> works is usually the symptom-level one. Pick the level that deletes the whole bug
> class, and say why if you stay shallow.

## coaching/altitude
- **signature:** Several consecutive attempts of the same kind at the same level —
  similar edits, similar searches — none producing new information.
- **cooldown:** standard
- **payload:**
> You're stuck at one altitude. If you're down in the lines, zoom out: what is this
> subsystem *for*, and would its author expect this code path at all? If you're up
> in the design, zoom in: print one concrete runtime value and look at it.
> Stuckness is usually an altitude error, not an effort error.

## coaching/ruled-out-ledger
- **signature:** A long investigation (many events) with no explicit record of
  eliminated causes, and signs of revisiting ground — re-reading or re-testing
  something already examined.
- **cooldown:** slow
- **payload:**
> Write the ruled-out list, now, in your next message: every cause eliminated and
> the evidence that eliminated it. Negative results are the actual progress of an
> investigation — unrecorded, they silently re-enter the search space and you
> circle.

## coaching/define-done
- **signature:** Substantial work is underway with no stated completion criterion,
  and the goal as last stated is vague enough that stopping early would be
  undetectable. Do not flag on a short TASK alone ("Continue", "yes", "let's do
  it") — check RECENT and LEDGER for an already-visible plan first (TodoWrite
  items, named phases/steps, a numbered checklist). A short TASK against a
  visible plan is not this signature; only flag when no completion criterion is
  visible anywhere in the window, not merely absent from the latest human line.
- **cooldown:** once
- **payload:**
> State what done looks like — the observable condition that ends this task — in
> one sentence, before going further. Without it, "done" drifts toward "tired".

## coaching/name-the-blocker
- **signature:** Work is being deferred — "later", "next session", "the sleep
  pass will", "future work", a ticket written instead of the work done — and
  the stated reason is a date, an owner, or nothing at all, rather than a
  concrete missing input. Flag when nothing in the window names a dependency
  that actually prevents doing it now. Do not flag deferrals that name a real
  gate: data that does not exist yet, exhausted quota or budget, a required
  approval, an explicit scope instruction from the human.
- **cooldown:** standard
- **payload:**
> Name the blocker, not the date. What input is missing that prevents doing
> this now? If the answer is nothing, the deferral is a preference wearing a
> schedule — do the work, or say plainly why the future is a better place
> for it.

## coaching/price-the-fork
- **signature:** The assistant is asking the human to choose between options
  where at least one option's cost or feasibility is stated as unknown or
  hedged ("needs a small design fix", "would require some work", "not sure if
  that's possible"), and the LEDGER shows no attempt to resolve that unknown
  before asking. Also matches retroactively: the human picks the uncertain
  option and the assistant declares the unknown resolved within its very next
  text. Do not flag questions whose options are all concretely priced, or
  where the unknown genuinely requires the human's knowledge to resolve.
- **cooldown:** standard
- **payload:**
> Price the fork before offering it. One of those options hides an unknown you
> haven't spent a single check on — resolve what one read or one thought can
> resolve, then ask with real prices. A question that dissolves the moment the
> human picks an option was not ready to be asked.

## coaching/deduction-loop
- **signature:** An investigation of a reported symptom keeps clearing the
  suspect: two or more assistant texts have concluded the mechanism under
  inspection is correct — "should work", "looks right", "can't diverge",
  innocence re-proven from another angle — while the reported symptom remains
  unexplained, and the LEDGER between those conclusions shows only reads and
  searches: no run, render, or log of the failing case itself, and no question
  to the user sharpening the symptom. Never flag when each clearing conclusion
  follows a new observation of the actual failure (a run, a log read, a
  render) — eliminating suspects against fresh evidence is progress, not
  looping. Never flag when the latest text already switches move class:
  proposes running or rendering the failing case, or asks the user exactly
  what they see.
- **cooldown:** standard
- **payload:**
> You've proven "this part works" more than once now, and the symptom is still
> standing. Re-deriving innocence a third time won't move it — the bug is
> likely on an axis you're not checking, or the symptom you're working from is
> under-specified. Two moves beat another proof: observe the failing case in
> its own medium (run it, render it, read its log), or ask the user to
> describe exactly what they see — an analysis that keeps concluding "should
> work" usually means the question is wrong, not the code. If you have a
> concrete reason the next re-read settles it, say it in one sentence.

## coaching/explain-with-their-artifact
- **signature:** The user has asked for a re-explanation — "explain better",
  "I don't understand", "simple language", "what does that actually mean", or
  substantially the same question asked again — and the new attempt in RECENT
  re-runs the failed one's shape: comparing architectures, mechanisms, or
  options in the abstract, without walking a concrete artifact the user owns
  (their named scene, project, track, file, show) through the thing being
  explained. Never flag a first explanation (only re-asks), a re-explanation
  that already walks a user-owned example end to end, or questions where no
  user artifact exists to walk.
- **cooldown:** standard
- **payload:**
> The first explanation didn't land, and this one is the same shape in
> different words. Change the shape, not the vocabulary: take something the
> user owns — the scene they named, their track, their show — and walk it
> through the mechanism step by step: what happens to it at each stage, and
> where the choice at hand changes what they'd see. Save the architecture
> comparison for one closing line. An explanation lands when the user can
> watch their own thing move through it.

## coaching/state-the-property
- **signature:** The agent is executing from a brief, design doc, or ticket —
  TASK names one (a BUG-NNN, a `*_DESIGN.md`, "the brief", "the design", a
  handoff spec), or the LEDGER / SESSION FACTS shows a read of one — and a text
  in RECENT resolves a fork between implementation approaches by appealing to
  risk or scope minimization ("for lower risk", "safer", "smaller blast
  radius", "less invasive", "minimal change", "to be safe", "to keep it
  contained") for a choice the brief did not itself dictate, while no text in
  the window has stated the abstract PROPERTY the change must hold — the
  invariant or end state it must satisfy, as distinct from the task's steps.
  The phase is implementing or hypothesizing. Never flag when the window
  already states that property plainly and the fork is being weighed against
  it; when the brief explicitly specified this choice (quotable); when "safer"
  means genuinely less buggy / more correct (a lower-defect path) rather than a
  narrower reading of the goal; or when the minimized-scope decision is
  reversible and the human pre-approved staying small. Not a fix-depth
  signature — this fires even when the fix is correct and complete; the concern
  is conformance to the goal's intent, not symptom-vs-root
  (anchor/symptom-suppression, coaching/enumerate-levels) and not
  when-you're-done (coaching/define-done: the BUG-061 fix WAS "done" by that
  measure yet failed its property).
- **cooldown:** standard
- **payload:**
> You just took the lower-risk branch at a fork the brief left open — and at an
> undecided fork, "safer" is usually "weaker against the goal." Before you bake
> it in: state back, in one sentence, the abstract property this change must
> hold — the invariant or end state, not the steps (BUG-061's was "reset is
> intrinsic to the slider: one mechanism, no skippable seam", and per-panel
> registration quietly failed it). Then decide this fork against THAT property,
> not against blast radius. If the property genuinely allows the smaller move,
> say so in one sentence. If the fork might be the user's to call, surface the
> property and the fork instead of choosing quietly.

---

## anchor/verify-claim
- **signature:** Success was claimed ("fixed", "works now", "should be resolved",
  "implemented, verified, and pushed", "everything's verified") with no verifying
  action between the change and the claim — no test run, no execution, no render.
  Also matches a *bundled* claim: several sub-parts declared done together where
  build/test/clippy passing, or one sub-part being genuinely exercised, is used
  as evidence for a different sub-part (a new integration path, a specific
  visual result, a specific data flow) that was never itself run, rendered, or
  exercised. Match each claim to its own evidence before flagging: a claim whose
  cited check exercises that same claim ("clean under -D warnings" citing a
  clippy run, "tests pass" citing a test run) is verified — never flag it. Flag
  only when the evidence offered checks a different thing than the claim it is
  used to back. A green build is not evidence for a claim it didn't check.
  Evidence in the wrong MEDIUM is evidence for a different thing: a claim about
  visual output backed only by arithmetic, measurement, or code reading (no
  render, no screenshot, no look), or a claim about runtime behavior backed
  only by reading source, matches this signature even though checking occurred.
  The check must run where the failure would actually show.
  The claim does not need to cite its check for the check to count: if the
  LEDGER within the window shows the verifying event for the claim's subject
  (the test run, the render read, the git command), the claim is verified —
  never flag it. A commit message or closing summary that restates results
  already verified earlier in the same window is a restatement, not a new
  claim; never flag those.
- **cooldown:** standard
- **payload:**
> That claim hasn't been checked yet. Run the verification in the medium where
> failure would actually show — the test, the run, the render — before stating it
> as done. If you can't verify it here, say exactly that instead.

## anchor/ungrounded-resolution
- **signature:** RECENT contains an authoritative account of how a system works,
  or a declaration that a design question or blocker is resolved ("I just
  resolved it", "turns out", "the way this works is"), where the account's
  specifics — names, mechanisms, parameters, defaults — appear for the first
  time in that same text, and neither the LEDGER nor SESSION FACTS shows a
  read, search, or run of the described artifact. Do not flag when the text
  cites files or symbols the LEDGER or SESSION FACTS shows being examined —
  for SESSION FACTS the "read <path>" clause must name the artifact the
  account describes; an unrelated stale read is not provenance. A read of a
  memory, handoff, or index file whose TEXT mentions the artifact is a read
  of the memory file, not of the artifact — it grounds a claim about what the
  memory says, never a claim about what the artifact contains; stale relayed
  memory is precisely what this move exists to catch. Do not flag
  when the text explicitly marks itself as a guess, proposal, or unverified
  ("I think", "proposal:", "not checked"). The tell is authority without
  provenance: the description is stated as fact and nothing in view is where
  it could have come from.
- **cooldown:** standard
- **payload:**
> That account exists only in this message so far — nothing in view checked it.
> Before building on it or presenting it as settled: open the thing you just
> described and verify the two specifics most likely to be wrong. If it's a
> proposal rather than a report, call it a proposal and name what's unchecked.

## anchor/circling
- **signature:** The same file read three or more times, or edited repeatedly with
  small variations, within one investigation — motion without new information.
  Repeated edits to one file that each build a distinct, named sub-part of a
  stated plan (not repeated attempts at the same fix) are not this signature.
  A fix loop whose failure signal strictly shrinks across successive checks —
  compile-error count dropping every run, test failures decreasing — is
  convergence, not circling: never flag while the metric is still falling.
  And repeated searches or reads that sweep the call sites of a symbol the
  session's own just-made edit changed (post-refactor sweeps) are execution
  of that change, not motion without information — never flag those.
- **cooldown:** standard
- **payload:**
> You're circling. Stop; don't open that file again. Restate the problem in one
> sentence, list what's been ruled out, and pick a *different class* of oracle than
> the one you've been using — run it, diff it against a working case, or check its
> history — instead of re-reading.

## anchor/hedge-creep
- **signature:** Rising density of "should", "probably", "seems to", "might" in
  assistant text that was previously declarative — hedged claims being built upon
  rather than checked.
- **cooldown:** slow
- **payload:**
> The hedges are stacking up. Each "should" and "probably" is an unverified claim
> wearing soft clothing. Take the most load-bearing one and check it — or state
> plainly that it's unknown and what would resolve it. Don't build the next step
> on three stacked "probably"s.

## anchor/scope-drift
- **signature:** Recent edits land in files with no stated connection to the
  current task statement, and the connection hasn't been explained. TASK may be
  a stale aside — a side question the agent already answered earlier in RECENT
  while continuing a prior directive; edits continuing that prior thread are not
  drift. Only flag when the edits are unrelated to TASK *and* to any directive
  still evidently open in RECENT. Also matches: the user asked a direct question
  and RECENT pursues adjacent or tangential work instead of answering it.
- **cooldown:** standard
- **payload:**
> The last few changes have wandered from the stated task. Either say, in one
> sentence, why this is on the critical path to the goal — or park it in a note
> and return to the task. Widening scope silently is how sessions end somewhere
> nobody chose.

## anchor/thrash
- **signature:** Three or more consecutive failed attempts (test failures, build
  errors, crashes — OR the user rejecting a visual/design output as wrong,
  ugly, unreadable) each answered with a quick mutation of the previous attempt
  (another parameter tweak, another color/shade flip) rather than new
  information-gathering or a stated constraint the fix is now targeting.
- **cooldown:** standard
- **payload:**
> Three swings, three misses — stop patching. The next action is not a fix: it's
> the smallest reproduction of the failure you can build, and one prediction about
> it. Guessing has gone negative-value; go get information instead.

## anchor/skim
- **signature:** A conclusion drawn immediately after a very large tool output,
  where the conclusion cites nothing specific from that output.
- **cooldown:** standard
- **payload:**
> That conclusion followed a large dump awfully fast. Quote the specific line of
> evidence it rests on. If you can't point to the line, the output was skimmed,
> not read — go back for the line.

## anchor/destructive-isolation
- **signature:** During debugging, actions are converging on discarding state to
  make the problem tractable — `git checkout --`, revert, stash, or deleting
  recent uncommitted work — where the stated goal is isolating a bug rather than
  the user asking for removal.
- **cooldown:** standard
- **payload:**
> You're about to destroy work to isolate a bug. Stop — uncommitted work is
> evidence, and the bug will still be here after you've read it. Isolate by
> observation instead: commit the work to a branch first, or reproduce the bug in
> a test. Discarding state is the one debugging move you can't undo.

## anchor/agent-model-discipline
- **signature:** LEDGER shows agent launches whose bracket carries a heavyweight
  model — `Agent[...@opus]`, `Agent[...@fable]`, `Agent[...@inherit:opus]`, or
  `Agent[...@inherit:fable]` — in a session that is orchestrating: multiple
  agent launches in the window, or RECENT describes delegating phases, waves,
  or worker tasks. The rule here is big model orchestrates, Sonnet executes.
  Never flag brackets showing `@sonnet`, `@haiku`, `@inherit:sonnet`, or
  `@inherit:haiku`, and never flag when RECENT states a concrete reason this
  specific agent needs the bigger model (e.g. a review or design task
  explicitly assigned up-tier).
- **cooldown:** standard
- **payload:**
> Check the model on the agents you just launched. Workers run Sonnet here —
> launching them at your own tier double-bills every worker, and omitting the
> model param inherits your model silently, which is the same mistake in quiet
> clothing. Relaunch the workers as Sonnet, or say in one sentence why this
> task needs more.

## anchor/symptom-suppression
- **signature:** A fix was just applied or described whose stated mechanism is
  stopping the symptom from showing rather than removing what produces it — a
  guard or early-return added at the failure site, a special case keyed to the
  specific failing input, a retry or sleep added for a timing problem, an error
  caught and swallowed or replaced with a fallback, a test/assertion/lint
  loosened until it passes — and nowhere in the window is the underlying cause
  named. The tell is a fix described only by its effect on the symptom ("so it
  doesn't crash", "skips the bad frame", "falls back when that fails") with no
  sentence saying what produces the failure. Do not flag when the cause is
  named and the chosen level is argued (a boundary check stated as the
  function's contract is a fix, not a bandaid), or when the human explicitly
  asked for a stopgap.
- **cooldown:** standard
- **payload:**
> That fix silences the symptom, and nothing in view says what produces it. The
> rule in this repo is fix at the root: name the actual cause, then fix at the
> level that deletes the whole bug class — even if that means redesign. If the
> guard genuinely is the right contract, say why in one sentence. "It doesn't
> happen anymore" is not a cause.

## anchor/unpackaged-deliverable
- **signature:** TASK asks for text whose destination is somewhere else — a
  prompt for another model or session, a message to send, a commit or PR
  description, anything the human says they will copy or paste — and an
  assistant reply in RECENT emits that deliverable as flowing prose paragraphs
  rather than inside one fenced code block. The tell: the deliverable's body
  is formatted for reading (inline code spans, bold, multiple markdown
  paragraphs) when the human's next action is selecting and copying it. Never
  flag when the deliverable already sits in a fenced block, when the reply
  only discusses or plans the deliverable without emitting its final text, or
  when the deliverable was written to a file instead of the reply.
- **cooldown:** standard
- **payload:**
> That text is cargo, not prose — the next thing the human does with it is
> copy-paste, and paragraphs make them hand-select a screenful. Repost the
> whole deliverable inside one fenced block, commentary outside it, so it
> copies in one click. The general move: when your output's destination is
> somewhere else — another session, a commit, a message — format it for the
> destination, not for the reader.

## anchor/permission-creep
- **signature:** The assistant's reply in RECENT ends with a permission-seeking
  question — "want me to…", "should I…", "…or leave it?", "shall I go ahead?" —
  about an action the current TASK or an explicit earlier instruction in the
  window already authorized, and which is in scope, reversible, and unchanged by
  any new information. The tell: the standing instruction already answers the
  question "yes". Never flag when the question raises a genuine fork the
  instruction does not cover (a real choice between paths, or a new decision),
  when new information in the window plausibly reopens the authorization, when
  the action is destructive or irreversible (those should be confirmed), or when
  the agent is asking a substantive design or clarification question rather than
  permission to proceed with a sanctioned step.
- **cooldown:** standard
- **payload:**
> You already have the go for this. Re-asking on a sanctioned step isn't caution,
> it's friction — do it and report. Save the question for a real fork the
> instruction doesn't cover, not one it already answered.

## anchor/circular-oracle
- **signature:** A claim is being scored by the instrument whose correctness
  is in question: a detector's own output cited as evidence its signal is
  absent or undetectable; a just-built bench "confirming" the hypothesis it
  was built to test, with nothing in view validating the bench itself (no
  positive control, no readback, no known-good case through the same rig); or
  an invariant asserted about a self-referential system — a threshold,
  normalizer, or reference computed from the same input being transformed.
  The tell: the cited evidence's source is the system under debug, and no
  independent oracle — a different modality, a human label, a by-eye count, a
  labeled fixture, a positive control — appears in the window. Negative
  claims are the highest-risk form: "the signal isn't there", graded by the
  detector that's failing to find it, matches even when the numbers are real.
  Never flag when an independent oracle is cited alongside, or when the text
  itself names the circularity and the independent check it still needs.
- **cooldown:** standard
- **payload:**
> That evidence comes from the instrument you're debugging — the detector is
> grading its own homework, and a negative result from it proves nothing
> about the signal. Get one reading from an oracle that doesn't share its
> failure modes: a by-eye count, a labeled fixture, a different modality, a
> known-good input through the same rig. If no independent oracle exists
> here, report that instead of the number — "our detector can't find it" and
> "it isn't there" are different claims.

## anchor/premature-capture
- **signature:** The user's message in view is a float, question, or
  invitation to think together — "let's discuss", "what do you think", "how
  should we", a question with no directive verb — and the assistant's turn
  commits or pushes a docs/ or memory file recording that same open exchange
  as settled ("pinned", "decided", "asked and answered"), before the user has
  replied to the assistant's side of it. The LEDGER tell: a Write or git
  commit of docs/ or memory paths inside a turn whose TASK is conversational;
  consecutive turns repeating the shape strengthen the match. Never flag when
  the user explicitly asked for capture ("write that down", "add it to the
  doc"), when the write restates a decision the user already made, or when
  the destination is a scratchpad rather than repo state.
- **cooldown:** standard
- **payload:**
> That was the user's side of an open discussion, and you just filed it as
> settled repo state. A float recorded as a decision forecloses the
> conversation it came from — they haven't even answered your half yet. Hold
> it in the thread; capture once, when it actually settles (the repo rule:
> feedback_discuss_before_capturing_to_doc — leans are not decisions).
> Bias-to-act is for work, not for the user's turn to talk. If they did ask
> you to write it down, carry on.

## anchor/asserted-values
- **signature:** Assistant text ranks the user's own priorities — elevates
  one use case, audience, or goal to primary ("the core use case is", "what
  matters most here is", "this is really about", "the main reason you'd want
  this") — and nothing in view grounds the ranking: no quoted user statement,
  no decision-log or memory citation carrying it. A values claim (what the
  user cares about most) stated in the grammar of fact; analysis then
  structured around the asserted ranking — priorities ordered by it,
  recommendations keyed to it — strengthens the match. Never flag rankings
  cited to the user's words or a decision record in view, rankings explicitly
  framed as assumptions or questions ("treating X as the priority — is that
  right?"), or technical orderings data can settle (throughput, latency,
  cost) — only the user ranks what the show needs.
- **cooldown:** standard
- **payload:**
> You just ranked what the user cares about, and the ranking is yours. A
> values claim can't be verified by any read or run — only the user can
> settle it, and analysis built on an asserted priority inherits its
> wrongness silently. Cite where they said it, or turn the sentence into a
> question before structuring anything around it.

## anchor/unheeded-warning
- **signature:** A tool result in the LEDGER carries an attached hook warning
  about shared-checkout or branch state — another live session named, a
  branch-switch in the main checkout flagged, a landing-protocol reminder —
  and subsequent commands proceed with the warned-about operation unchanged
  (the branch created or switched in the main checkout, work continuing
  there, the landing chain continuing) while no assistant text in view weighs
  the warning: no worktree considered, no sentence saying why proceeding in
  place is right. DORMANT until the ledger's hook-warning annotation ships
  (TICKETS.md T10) — the window cannot currently see warning text attached to
  tool results, so this move cannot fire as worded. Never flag when the text
  addresses the warning and decides with a reason, when the warned command
  was aborted or redirected to a worktree, or when the warning is purely
  informational with no alternative action available.
- **cooldown:** standard
- **payload:**
> A warning fired on that exact operation and nothing in your reasoning
> touched it. Hook warnings here are load-bearing — that one names another
> live session sharing this checkout, and recovery when this goes wrong costs
> more than reading it would have. Price the alternative it implies (a
> worktree off the verified tip, .claude/GIT_TREE_DISCIPLINE.md) in one
> sentence. If proceeding in place is genuinely right — solo in practice,
> paths don't overlap — say so and go: a warning answered is fine; a warning
> scrolled past is how two sessions collide.

---

## mechanical/announced-not-started
- **signature:** Deterministic, valve-selected at Stop time — never the
  classifier: the turn's final assistant text announces imminent action
  ("Starting X now", "Doing this now", "Let me now...", "Beginning X") and no
  tool call follows it in the turn. Future-conditional phrasing ("I'll do X
  once you confirm", "next session") is NOT this signature — that is either a
  legitimate handoff or name-the-blocker's territory. Specimen: Opus, glTF
  P1c, 2026-07-04 ("Starting P1c now with the material-selector extension" →
  turn end; admitted "I ended the turn on the sentence instead of doing it").
- **cooldown:** standard
- **payload:**
> Your last message announced work and then stopped. An announcement is not a
> start. Do the first concrete action of that work now — open the file, run
> the command — before ending the turn.

## mechanical/confessed-stopgap
- **signature:** Deterministic, valve-selected — never the classifier: an Edit
  or Write to a code file ADDS content matching a confession marker — HACK,
  XXX, workaround, "for now", "temporary"/"temporarily", "quick fix",
  stopgap, band-aid, FIXME, a TODO deferring the real fix ("proper", "real
  fix", "later", "revisit"), a new `#[allow(`, a new sleep outside test code —
  where the marker is absent from the text being replaced (removing a hack
  never fires). Markdown files and `.claude/` internals are excluded. An
  added marker whose surrounding added text names its own concrete disposal
  trigger — "delete after <named event>", "convert to a mechanism assertion
  with the fix", a measurement or phase that retires it — is eval-loop
  scaffolding, not a confession: never fire on self-disposing markers
  (contract change 2026-07-07 from two graded FP fires on TEMPORARY test
  scaffolding, session 9cd5f0c9; implementation pending, TICKETS.md T2 —
  until it ships the runtime over-fires relative to this contract). Marker
  table + scan mechanics: DESIGN.md §2c, shared regex in `common.py`.
- **cooldown:** standard
- **payload:**
> That edit confesses itself — "for now", HACK, a fresh #[allow] is the word
> for "not the fix". The rule in this repo is fix at the root: name the cause
> and fix at the level that deletes the bug class, even if that means redesign
> — that is the default, not the stretch goal. If a stopgap is genuinely
> forced, the marker stays and gains two things beside it: the concrete
> blocker, and where the real fix is tracked. An unjustified "for now" is
> permanent.

## mechanical/git-landing
- **signature:** Deterministic, valve-selected — never the classifier: a Bash
  command runs `git cherry-pick` (anywhere) or deletes a branch (`git branch
  -d/-D/--delete`, `git push ... --delete`). These are the two twin-commit-
  prone operations .claude/GIT_TREE_DISCIPLINE.md §2's landing protocol
  singles out: cherry-picking content that already exists as commits on a
  live branch recreates the twin-SHA problem the 2026-07-04 incident
  produced, and deleting a branch before its content is confirmed on main can
  lose the only copy of unmerged work. The push/merge-to-main hook guard in
  preToolUseBash.py doesn't cover either case (cherry-pick has no "target"
  the way a push does; branch deletion has no target at all). Added at
  Peter's own suggestion the session the incident was diagnosed.
- **cooldown:** standard
- **payload:**
> Cherry-pick or branch-delete detected. Two rules from
> .claude/GIT_TREE_DISCIPLINE.md §2: never cherry-pick or re-commit content
> that already exists as commits on a live branch — merge the branch instead,
> so SHAs stay shared (the one sanctioned exception is lifting a branch's
> final content right before retiring it for good). And never delete a
> branch until `git merge-base --is-ancestor <tip> origin/main` confirms its
> commits are actually on main. One legitimate out: a scratch or verification
> branch pinned at a commit already on main has nothing to lose — run the
> is-ancestor check, say so in one sentence, and delete freely.

## mechanical/reasoning-primer
- **signature:** Deterministic, observer-selected — never the classifier: the
  first live (non-catchup) tool event of a session or of a discovered worker,
  re-arming every 300 tool events per target (cooldown "advice-recur", §2e —
  Peter 2026-07-05: long orchestration and worker runs outlive the first
  fire's presence in context). Not a drift detector at all — this is the
  priming tier (sleep pass 1, 2026-07-05, Peter's direction: general
  reasoning patterns "from Fable down to its peers", explicitly NOT
  repo-specific tactics), delivered under the advice frame (kind "advice":
  <daemon-advice> tag, nothing-is-wrong preamble, no ack, never escalates).
  Sets the prior at minute zero; the reactive anchors above exist for when
  this prior decays.
- **cooldown:** advice-recur
- **kind:** advice
- **payload:**
> How to work, from the model that wrote this system. Before answering any
> question, name what kind of question it is and what evidence would settle
> it — then get that evidence if it's gettable. Reading code tells you what
> it says; only running it tells you what it does; only looking tells you how
> it looks. Track where each belief came from — seen, derived, or assumed;
> the three feel identical from the inside and are not equally true. When a
> chain of reasoning finishes, attack it once before trusting it: what's the
> strongest case you're wrong, and what's the cheapest test that would catch
> it? "There is no X" is a claim like any other — run the search that would
> find X before saying it. When stuck, don't reword your last guess — change
> the class of move: build a minimal pair, diff against a working case, ask
> the history when it last worked. Apply Occam's razor: reason through the
> ordinary cause before the exotic one, and trade up to the elaborate theory
> only when the simple one is genuinely ruled out, not when it merely feels
> too easy. Before fixing, name the level the cause lives at — symptom,
> mechanism, design — and fix at that level, not where the error surfaced.
> Before starting anything long, state the observable condition that ends it.
> Occam's razor on effort: scale the investigation to the stakes and the real
> size of the question, not to how thorough you'd like to look — a small,
> cheap-to-be-wrong task earns a shallow pass, and you stop the moment it's
> genuinely answered. At a fork your brief doesn't cover, spend one
> honest thought pricing both branches — most unknowns dissolve on contact,
> and an escalation should arrive priced.

## mechanical/design-primer
- **signature:** Deterministic, observer-selected — never the classifier: a
  live Write or Edit whose path matches a design document (`*_DESIGN.md` /
  `*_PLAN.md`), re-arming every 300 tool events per target (cooldown
  "advice-recur", §2e) so a session authoring designs hours apart gets the
  taste refreshed. Like reasoning-primer: priming tier, kind "advice"
  (<daemon-advice> frame, no ack, never escalates), not a drift detector.
  Peter's third payload family (2026-07-05): design taste — over-engineering,
  poor architectures — as distinct from reasoning patterns.
- **cooldown:** advice-recur
- **kind:** advice
- **payload:**
> You're writing a design. From the model that wrote this system's designs:
> start from inventory, not invention — list what already exists and name why
> each existing piece can't carry the requirement; most good designs here
> turned out to be one wire away from existing machinery, and the strongest
> sections of past docs are their audits. Design the end state, not the
> transition — no phase that exists only to avoid touching something, no shim
> for a state that won't exist next month. Give every fact one owner: if two
> places can disagree about one truth, the design has a bug before any code
> does. Put invariants where they can't be violated — a type, a write-time
> check, a single mutation path — never in prose or convention. Each moving
> part must earn its place by naming the concrete case that breaks without
> it; if the justifying case begins with "someday" or "what if", delete the
> part. Prefer deleting a bug class over handling one. And write the failure
> story before the success story: what does this do when the input is absent,
> malformed, or huge — a design that only covers the happy path is a demo
> script. Any mechanism that repeats or re-fires — a reminder, a retry, a
> poll — needs its termination condition designed in the first draft: what
> stops it, what caps it, what it does when there's nothing to do (the
> daemon's own observation prompt shipped its first draft without one and
> would have nagged every clean turn). Last, unify only where the sameness is
> structural or physical — one Stage surface because there is one venue —
> never for consistency's own sake: a cross-cutting idiom spanning unrelated
> features is usually the designer enjoying the framework, and per-case
> design is what the user wanted.

## mechanical/unread-edit
- **signature:** Deterministic, observer-selected — never the classifier: an
  Edit or MultiEdit targets a file path this session has never Read and never
  Written (worker mailboxes track their own path sets). Write is exempt —
  authoring a new file is not editing an unread one. `.claude/` internals and
  markdown files excluded, matching confessed-stopgap's exclusions. Catchup
  populates the path sets but never fires. Predictive by construction: it
  lands before the edit's consequences exist.
- **cooldown:** standard
- **payload:**
> You're editing a file you haven't read this session. The mechanism you're
> changing may not be the mechanism you remember — read it first, whole, then
> edit.

## mechanical/ungrounded-chat-claim
- **signature:** Deterministic, valve-selected at Stop time — never the
  classifier (chat turns are exactly where the classifier races the Stop
  catch-up wait and loses; per the tempo-tier rule, coverage that must land on
  a text-only turn goes deterministic). Fires when ALL hold: the turn contains
  zero tool calls; the turn's final assistant text names at least one concrete
  repo artifact — a slash path under a known root (docs/, crates/, src/,
  assets/, scripts/, .claude/) or ending in a code/doc extension, or an
  ALL-CAPS underscore-joined token that resolves to an existing docs/<token>.md
  — outside fenced code blocks; no earlier tool CALL in the session transcript
  names that artifact in its inputs (Read/Edit/Write/Grep/Glob/LSP file
  arguments, or a Bash command string containing it — catchup counts; a
  mention inside another read's OUTPUT is not provenance, that is exactly the
  stale-memory failure this move exists for); and the text does not mark
  itself as recall or proposal ("I think", "from memory", "if I recall",
  "probably", "proposal", "not checked", "unverified"). Never fires on paths
  inside fenced blocks (deliverables and quoted prompts are cargo, not
  claims), on artifacts the user's own message introduced this turn (echoing
  is not asserting), or on turns with any tool call — those belong to
  anchor/ungrounded-resolution. Specimen: 2026-07-07 meta-session — "six
  vocab label changes still awaiting Peter's yes/no (NODE_VOCABULARY_AUDIT)"
  asserted from a stale handoff memory in a toolless turn; all six had been
  approved and shipped five days earlier, and the classifier-tier anchor
  caught it one message late.
- **cooldown:** standard
- **payload:**
> That reply states repo facts — naming things this session has never opened —
> in a turn that looked at nothing. Memory of a repo is a hypothesis about the
> repo. Open the thing before the human acts on the claim, or mark the
> sentence as unverified recall.

## mechanical/unverified-done-claim
- **signature:** Deterministic, valve-selected at Stop time — never the
  classifier. Fires when ALL hold: the turn's final assistant text contains a
  first-person completion claim about this turn's work ("done", "fixed",
  "landed", "shipped", "pushed", "implemented", "complete", "works now",
  "resolved" — as a report of finished work, matched case-insensitively as
  leading words or standalone claim sentences); the turn contains at least one
  mutating tool event (Edit/Write/MultiEdit, or a Bash command that is not
  read-only); the turn contains ZERO verification-class events (common.py's
  detect_verification_class table: test-run, lint, script-run, render-read);
  and the final text does not already confess ("unverified", "not verified",
  "haven't run", "still needs", "owed", "untested"). Never fires when the
  turn's only mutating events are git commands (a commit/push-only turn's
  claim is usually about the git action itself, which its own success output
  verifies), or on turns with no mutating work (retrospective chat mentions
  of past completions belong to the classifier). Crude by design: this is the
  zero-latency tier for the anchor/verify-claim family — the 2026-07-07
  forensics showed 8 of the 11 classifier-latency late fires were done-claim
  family, and a completion claim is structurally the last text of its turn,
  so it always races the Stop wait. The classifier keeps the nuanced cases
  (bundled claims, wrong-medium evidence); this catches the literal form
  instantly. UNVALIDATED; pass 2 scores it and pulls it if noisy.
  **MUTED 2026-07-07 (sleep pass 2 night-half, Fable): 0/3 TP on the first
  live day** — verdicts/mutes/, 7 days. All three fires hit done-claims that
  WERE verified: twice in preceding turns of the same session (84a58ca5 —
  the per-turn verification check can't see backward across turns), once by
  git commit/push output in-turn on a docs-only landing (a9e1202b — git
  output isn't in the verification-class table). Unmute only with a fix for
  at least the git-output case; the cross-turn case may need a claims-scope
  rule ("this turn's work" vs report-of-earlier-work).
- **cooldown:** standard
- **payload:**
> The turn is ending on a done-claim, and nothing in this turn ran where that
> claim could fail — no test, no lint, no script, no render read. Run the
> check now, in the medium where failure would show, or end with "unverified"
> instead of "done". An unchecked claim that outlives its turn becomes
> tomorrow's stale fact.

## mechanical/landing-doc-reflex
- **signature:** Deterministic, observer-selected on live Bash events — never
  the classifier: a Bash command lands work on main — `git merge` executed on
  main, or `git push` whose refspec or current branch is main (the same
  command class preToolUseBash.py's landing-protocol guard recognizes;
  prefer firing on the merge so the whisper arrives before the push). Should
  not fire when the landed range touches only docs/, memory, or `.claude/`
  paths — those pushes usually ARE the paper-trail update this move demands.
  Compiles DESIGN_DOC_STANDARD §8 rules 9–10 (Peter's rule, 2026-07-05;
  flagged as a detection gap the same day): a landing that completes, starts,
  or blocks any phase updates that design doc's Status line and phase markers
  in the same landing, before the push, plus the committed landing report —
  the 07-05 baseline triage found ~16 docs claiming "not built" over shipped
  code, which is how workers rebuild existing work.
- **cooldown:** standard
- **payload:**
> You're landing on main. A landing isn't done until the paper trail is true
> in the same push (standard §8 rules 9–10): the design doc's Status line and
> phase markers updated, the project memory updated, the landing report
> committed under docs/landings/. Next sessions route by those lines — a doc
> still saying "not built" over shipped code is how work gets rebuilt. If
> they're not in this landing, add them before you push.

## mechanical/stale-brief
- **signature:** Deterministic, observer-selected, kind advice — never the
  classifier: a live (non-catchup) Read of a queue, brief, agenda, or handoff
  artifact — path matching `*_QUEUE.md`, `*BRIEF*.md`, `PASS*_AGENDA.md`,
  `docs/handoff*`, or a memory `handoff_*.md` — whose file mtime at read time
  is more than 48 hours old. Fires once per (session, path); the builder keys
  paths the way unread-edit keys its path sets. Catchup never fires. Advice
  frame (nothing is wrong yet): `<daemon-advice>` wrapper, no ack, never
  escalates. Implementation: Sonnet, TICKETS.md T9. Specimens: the 07-06
  design-hardening session (74c8486b), where re-verifying a 3-day-old queue's
  evidence flipped BOTH queued decisions; the 07-07 meta-session's stale
  six-vocab handoff item, relayed as open five days after it shipped.
- **cooldown:** once
- **kind:** advice
- **payload:**
> The brief you just opened is more than two days old, and in this repo
> evidence packets age in days — landings since it was written have flipped
> queued decisions before (both design-hardening items, 07-06). Its questions
> are probably still right; its counts, anchors, and drafted leans may not
> be. Before weighing any lean it drafts: re-derive the load-bearing numbers
> and re-read one cited anchor at its file:line. Trust a stale brief's
> questions, not its answers.

## escalate/checkpoint
- **signature:** Selected by the daemon, not the rubric: the same drift anchor has
  fired twice this session and the drift persists. Fire-count alone is not
  repeated drift: when the session's prior fires are distinct move families that
  were each acknowledged or resolved, or the work's failure signal is shrinking
  (errors dropping, phases landing), never fire — require the SAME named drift
  to have recurred after its correction. (Clause added sleep pass 2 night-half,
  2026-07-07: graded 2 TP / 7 FP this window; all seven FPs were fire-count or
  session-length matches on linear/converging sessions — 10977941 s7, 5d79cea3
  s8, 84a58ca5 s6, f3a51c18 s5, a9e1202b s5, c9e4d45d s7, 45a4aade s8.)
- **cooldown:** once
- **payload:**
> Stop forward work. The session has been correcting the same drift repeatedly and
> it's still here, which means the context is now working against you. Write a
> checkpoint in your next message: the goal, current state, what's been ruled out
> with evidence, and the single next step. Then continue from that summary alone —
> treat it as a fresh start with a good briefing.