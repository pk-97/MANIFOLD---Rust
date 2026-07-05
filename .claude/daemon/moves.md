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
  account describes; an unrelated stale read is not provenance. Do not flag
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
  never fires). Markdown files and `.claude/` internals are excluded. Marker
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
> commits are actually on main.

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
> script.

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