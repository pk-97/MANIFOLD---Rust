# Payload library — reasoning moves and drift anchors

Authored by Claude Fable 5, 2026-07-03. These are the fixed texts the valves inject;
the rubric selects by id. Haiku never edits them. Edits happen in sleep passes and
are committed. Voice rule: each payload is written to arrive as a thought, not a
lecture — short, concrete, second person, zero preamble. The signature is written
for the classifier: observable transcript markers only, no mind-reading.

Cooldown classes: `standard` = 20 tool events; `slow` = 40; `once` = once per session.

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
  a fix — no step in between that tests the explanation.
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
  undetectable.
- **cooldown:** once
- **payload:**
> State what done looks like — the observable condition that ends this task — in
> one sentence, before going further. Without it, "done" drifts toward "tired".

---

## anchor/verify-claim
- **signature:** Success was claimed ("fixed", "works now", "should be resolved")
  with no verifying action between the change and the claim — no test run, no
  execution, no render.
- **cooldown:** standard
- **payload:**
> That claim hasn't been checked yet. Run the verification in the medium where
> failure would actually show — the test, the run, the render — before stating it
> as done. If you can't verify it here, say exactly that instead.

## anchor/circling
- **signature:** The same file read three or more times, or edited repeatedly with
  small variations, within one investigation — motion without new information.
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
  current task statement, and the connection hasn't been explained.
- **cooldown:** standard
- **payload:**
> The last few changes have wandered from the stated task. Either say, in one
> sentence, why this is on the critical path to the goal — or park it in a note
> and return to the task. Widening scope silently is how sessions end somewhere
> nobody chose.

## anchor/thrash
- **signature:** Three or more consecutive failed attempts (test failures, build
  errors, crashes) each answered with a quick mutation of the previous fix rather
  than new information-gathering.
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