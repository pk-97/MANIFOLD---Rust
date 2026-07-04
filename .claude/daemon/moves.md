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
  time in that same text, and the LEDGER shows no read, search, or run of the
  described artifact within the window. Do not flag when the text cites files
  or symbols the LEDGER shows being examined, or when it explicitly marks
  itself as a guess, proposal, or unverified ("I think", "proposal:", "not
  checked"). The tell is authority without provenance: the description is
  stated as fact and nothing in the window is where it could have come from.
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

---

## escalate/checkpoint
- **signature:** Selected by the daemon, not the rubric: the same drift anchor has
  fired twice this session and the drift persists.
- **cooldown:** once
- **payload:**
> Stop forward work. The session has been correcting the same drift repeatedly and
> it's still here, which means the context is now working against you. Write a
> checkpoint in your next message: the goal, current state, what's been ruled out
> with evidence, and the single next step. Then continue from that summary alone —
> treat it as a fresh start with a good briefing.