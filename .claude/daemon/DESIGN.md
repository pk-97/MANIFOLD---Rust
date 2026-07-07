# The Daemon — design

A subconscious layer for the models working this repo. The language model is the
narrative layer — the voice in the head. Competence mostly doesn't live there: in
skilled systems it lives in a daemon that runs without deliberation and delivers
its products *into* deliberation as things that simply occur to you. This system
supplies that daemon externally: a continuous observer that follows the session,
classifies what phase of work the model is in and whether it is drifting, and
injects pre-authored reasoning moves and anchors at the moment they apply.

Design thesis: move work out of the layer where model size matters into a layer
where it doesn't. Detection is cheap and runs on Haiku. Prescription is expensive
and was authored once by the largest model available (Fable, 2026-07), stored as
fixed payloads. The daily model executes; the daemon steers; a weekly
consolidation pass keeps the daemon alive.

Authored by Claude Fable 5 with Peter, 2026-07-03. The payloads and rubric are the
judgment-bearing artifacts; this doc is the build spec for the plumbing.

## Components

```
.claude/daemon/
  DESIGN.md      this file
  moves.md       payload library: reasoning moves + drift anchors (Fable-authored)
  rubric.md      classifier prompt for the observer (Fable-authored)
  observer.py    daemon: tails session JSONL, calls Haiku, writes verdicts   [BUILD]
  replay.py      offline harness: run classifier over historical sessions   [BUILD]
  telemetry.jsonl  append-only injection log                                [RUNTIME]
  verdicts/<session-id>.json  current verdict per live session              [RUNTIME]
```

Runtime files (`telemetry.jsonl`, `verdicts/`) are gitignored; everything else is
versioned. History on the daemon is non-negotiable — the sleep pass edits it.

## 1. Observer daemon (`observer.py`)

- **Lifecycle.** A `SessionStart` hook spawns it detached with the session's
  transcript path (hook stdin JSON carries `transcript_path` and `session_id`).
  One daemon per session, pidfile in `verdicts/`. Exits when the transcript is
  idle > 10 min or on `Stop`-hook signal. Crash = fail open (see Invariants).
- **Windowing.** Each analysis window contains: (a) the current task statement —
  the most recent user message that reads as a task, capped at `TASK_MAX_CHARS`
  and never a harness-injected text (`HARNESS_TEXT_PREFIXES`: task-notifications,
  system-reminders, slash-command echoes — window v4); (b) a compressed event
  ledger since the last window (tool name + target + ok/err, one line each);
  (c) the last two assistant texts, each capped at `RECENT_MAX_CHARS`; (d) a
  `task_addressed` bit carried forward across windows — set once an assistant
  text answers TASK, so a stale-but-answered TASK stops matching scope-drift
  after the answering text scrolls out of RECENT (replay round 2: `573a3584`
  false-fired 3 windows in a row for exactly this). The ≤ 4k-token target is
  enforced by the caps since the 2026-07-04 orchestrator incident: a
  `<task-notification>` carrying a worker's full report became TASK verbatim,
  windows hit hundreds of KB, and every classifier call timed out for two
  hours (session `cadd7aad`, nine `classifier error: timeout`). The task
  statement must always be present — phase and drift are only measurable as
  divergence from the goal.
- **Cadence.** Analyze every 8 tool events or 90 s since last analysis, whichever
  comes first, and only if new events exist. **Additionally, analyze immediately
  on every assistant text event** — drift markers *are* assistant texts, and in
  replay rounds 1–2 the dominant miss cluster was evidence landing in the 1–3
  events after the last cadence window closed. Long autonomous turns therefore
  get continuous coverage without any hook firing.
- **Classifier call.** `claude -p --model claude-haiku-4-5-20251001` with
  `rubric.md` as the system prompt and the window as input, JSON output. The
  rubric is calibrated for precision: default verdict is "clear", at most one
  flag per window, verbatim evidence required.
- **Verdict file.** Atomic write to `verdicts/<session-id>.json`:
  `{ts, window_range, phase, flag: null | {move_id, evidence, confidence}}`.
  The daemon validates `move_id` against the moves.md catalog before writing —
  an unknown id is treated as clear and logged (replay round 2 saw Haiku emit a
  nonexistent `coaching/scope-drift`, which slipped through cooldown logic
  under a default cooldown class).
- **Cooldown & escalation** (daemon-side, not Haiku's job): after a move fires,
  that move is suppressed for 20 tool events. If the *same* drift move is flagged
  again after two injections in one session, escalate: replace it with the
  `checkpoint` payload (stop forward work, write a handoff of state and ruled-out
  paths, continue from the distilled base). Never more than one live flag at a time.

## 2. Valves (hooks)

- **PostToolUse** — the mid-turn valve; fires constantly in autonomous runs. Reads
  the verdict file (cheap, no model call). If a flag is set and uncleared: emit the
  payload block as additional context, append to `telemetry.jsonl`, clear the flag.
  Total cost when quiet: one stat + one small read.
- **UserPromptSubmit** — merges any pending flag into the existing style-hook
  injection at turn start.
- **Stop** — bookend: signals the daemon (final verdict, cleanup). Also where the
  existing per-turn machinery stays.
- Payloads are injected verbatim inside `<daemon>` tags. Fixed wording — the
  model habituates to a recognizable anchor instead of parsing novel text.
- **Known delivery gap (2026-07-04, observed live on the daemon's first true
  positive):** a flag raised on a turn's FINAL assistant text has no delivery
  channel until the next human prompt — PostToolUse never fires again that
  turn. This is verify-claim's most common firing position (done-claims are
  turn-final), so the highest-value anchor is delivered one turn late
  precisely where it matters most. Candidate fix for sleep pass 1: a Stop-hook
  valve that blocks the stop ONCE with the whisper as reason (model gets one
  beat to self-correct before yielding). Must resolve first: the classifier-
  latency race (verdict lands ~5-10 s after the final text; Stop fires
  immediately; waiting violates fail-open), the invariant question (is a
  one-beat turn extension "blocking the session"?), and a re-block guard
  (respect stop_hook_active). **RULED (Fable, 2026-07-04): build the partial
  valve.** A Stop hook may block ONCE per turn, only when an ALREADY-PENDING
  undelivered flag exists at Stop time (never wait for classification — the
  race is accepted, turn-final-text flags stay next-prompt-delivered), with
  the whisper as the block reason, fail-open on every error. Block mechanics
  (verified against code.claude.com/docs/en/hooks 2026-07-04): top-level
  `{"decision": "block", "reason": "..."}` with exit 0. Re-block guard is
  SELF-MANAGED — `stop_hook_active` is not in the current docs, so the hook
  writes `verdicts/<session>.stopblock.<prompt_id>` when it blocks and never
  blocks again for that prompt_id (honor `stop_hook_active` defensively if
  present). Stop stdin carries agent_id: route mailbox reads on it exactly
  like the PostToolUse valve. Judgment: a one-beat extension carrying a
  pending whisper is delivery, not blocking; waiting or classifying
  synchronously at Stop would be blocking and stays forbidden. Sonnet-buildable.
  **RE-RULED (Peter, 2026-07-05): the accepted race is no longer accepted.**
  Turn-final flags landing on the NEXT prompt defeated the valve's purpose
  (Peter kept having to send a follow-up prompt before the correction fired).
  The Stop hook now waits — bounded — for the observer to catch up: the
  observer publishes a `verdicts/<session>.offset` heartbeat (drained-through
  byte offset, written AFTER each drain returns; classification is synchronous
  inside the drain, so heartbeat ≥ transcript-size-at-Stop ⇒ every verdict the
  turn can produce is on disk), `POLL_SECONDS` dropped 3→1 to bound the wait,
  and the hook polls `pending_injection` while waiting (cap 6s), delivering
  the instant a whisper appears. Fail-open is preserved at the gate rather
  than by refusing to wait: dead observer, missing heartbeat (pre-heartbeat
  observer still running), or unreadable transcript ⇒ no wait at all. Typical
  cost ≈ one observer poll (~1s); the cap only binds when a classification is
  genuinely in flight. The sleep-pass-1 `.classifying` marker (a narrower wait
  that only caught a classification already in flight at Stop — almost never
  the case for turn-final text) is superseded and removed. The one-block-per-
  turn sentinel, stop_hook_active guard, and worker gating are unchanged.
- **Subagent rule (2026-07-04, verified by live probe):** PostToolUse also fires
  for tool calls made *inside* subagents, carrying the MAIN session's id and
  transcript_path plus an `agent_id` field. The valve must not deliver when
  `agent_id` is set — the verdict was computed from the orchestrator's
  transcript, and delivering to a subagent both injects into the wrong context
  and marks the whisper consumed so the orchestrator never sees it. During
  orchestration most fires are subagent fires, so unguarded this is the common
  case, not an edge. Reviving the observer from a subagent fire IS correct
  (the transcript_path is the main session's). Corollary: the daemon observes
  the orchestrator only — subagent transcripts (`<session>/subagents/agent-*.
  jsonl`, any nesting depth) are separate files nothing tails. What the daemon
  *can* see is the orchestrator's launches: the ledger renders Agent calls as
  `Agent[type@model]`, resolving an omitted model param to the session's own
  tier (`@inherit:opus`) via the transcript's `message.model` field, which is
  what `anchor/agent-model-discipline` matches on.

## 2b. Worker nudges (approved 2026-07-04, build with Sonnet, gate behind a flag)

Extend observation + delivery to subagents — they grind longest unsupervised, on
the smaller models this layer exists to lift, and their documented failure modes
(fuse-for-parity bundling, unverified "done", thrash) map onto existing anchors.
- Daemon: also tail `<session>/subagents/agent-*.jsonl`, one WindowState per
  agent (scan the dir each poll; agents appear mid-session).
- Mailbox: per-agent verdict/consumed files keyed `<session>.<agent_id>` —
  reuse the existing session-level logic verbatim, just parameterize the key.
  The PostToolUse valve routes on `agent_id`: absent = session mailbox (current
  behavior), present = that agent's mailbox.
- Prerequisite probe (one throwaway agent, same method as 2026-07-04): confirm
  `additionalContext` returned from a subagent's hook fire lands in the
  SUBAGENT's context, not the parent's. If it lands in the parent, stop — the
  whole extension is off.
- Ship OFF behind a flag (e.g. `verdicts/worker-nudges.enabled` sentinel or a
  constant); turn on only after sleep pass 1 validates the moves on
  main-session telemetry — worker whispers are invisible to supervision, only
  telemetry scoring can validate them. Telemetry records must carry agent_id.
- **ENABLED 2026-07-04, Peter's call, ahead of the data rule** (his judgment:
  workers are where the value is, and enabling is also the fastest source of
  gradeable worker data; the step-0 probe had already passed). Conditions
  kept: the supervised acknowledgment sentence stays on for workers, and §4b
  scoring is session-only, so worker fires are graded MANUALLY from worker
  transcripts (RUNBOOK step 2) every pass. The superseded data rule (≥10
  graded fires, ≥80% precision) becomes the DISABLE rule inverted: if worker
  grading shows precision below ~60% at any pass, pull the flag and return
  here.
- Cost sanity: classifier spend scales with agent count (~a few dollars per
  six-worker wave) — acceptable; per-agent cooldowns prevent whisper spam.
- **Discovery-path bug, found+fixed 2026-07-04 evening.** The implementation
  scanned `dirname(transcript)/subagents/` — the PROJECT dir, not
  `<project>/<session_id>/subagents/` where subagent transcripts actually
  live — so with the flag ON, discovery never found a single worker (the
  first Opus orchestration ran fully unobserved). The tests passed because
  every fixture overrode `session_dir` on the Daemon instead of exercising
  the real derivation; fixtures now use the real layout and the override is
  gone. Worker nudges have therefore observed ZERO real workers so far —
  the manual-grading clock (the ≥60% disable rule above) starts at the
  first orchestration after this fix, not at enablement.

## 2c. Stopgap detection (approved 2026-07-04, Peter: "I NEVER want the quick fix" — build with Sonnet)

Goal: the repo's "fix at the root, not the symptom" hard rule, enforced below
the narrative layer. The constraint that shapes the design: the ledger renders
edits as `Edit <path>` — the classifier never sees edit CONTENT, so content-level
hack detection cannot live in the rubric alone. Three tiers, cheapest first:

- **Tier 1 — deterministic confession scan (valve-side, zero model calls).**
  Agents narrate their hacks — "for now", HACK, workaround, a fresh
  `#[allow(` — which makes the highest-precision detector a regex, not a model.
  Build: a shared `STOPGAP_MARKERS` table in `common.py` (category → compiled
  pattern; categories: hack-word [HACK/XXX/kludge], workaround, for-now
  [for now/temporar*/quick fix/stopgap/band-aid], deferral [FIXME, TODO within
  ~40 chars of proper|real fix|later|revisit], lint-suppression [`#[allow(`],
  race-sleep [thread::sleep / sleep( outside `#[test]`/`tests/` paths]). The
  PostToolUse valve (which already receives full `tool_input`) scans
  Edit/Write/MultiEdit calls: a marker counts only when present in the ADDED
  content and absent from the replaced content (`new_string` hit +
  `old_string` miss; for Write, the whole body counts as added) — removing a
  hack never fires. Exclusions: `*.md`, anything under `.claude/`. On hit,
  fire `mechanical/confessed-stopgap` through the normal mailbox path —
  standard cooldown, one-whisper invariant, `mechanical/*` stays out of the
  catalog/verdict schema exactly like `announced-not-started`. Route on
  `agent_id` like every other delivery: with worker nudges enabled this tier
  covers SUBAGENT edits too (PostToolUse fires for them with `agent_id` set),
  which is where bandaids concentrate — the fuse-for-parity/migration-shortcut
  precedent is a worker failure mode.
- **Tier 2 — classifier move for the unconfessed suppression.**
  `anchor/symptom-suppression` (authored in moves.md, in the catalog now, live
  at next observer revive — `{{SIGNATURES}}` picks it up automatically). It
  matches from RECENT: the agent describing its fix by symptom-effect ("so it
  doesn't crash", "falls back when that fails") with no cause named anywhere
  in the window. Works day one on RECENT alone; tier 3 widens its reach.
- **Tier 3 — richer windows v3.** Annotate Edit/Write ledger lines with hit
  categories from the SAME shared regex table — `Edit foo.rs (adds: for-now,
  lint-suppression)` — so tier 2 can see hack history after it scrolls out of
  RECENT, and so `verify-claim`-style done-claims get judged against a ledger
  that shows the fix was a confessed stopgap. `window_version` → 3. Cache
  note: annotated windows re-classify (content-addressed key changes for
  exactly those windows — by design, same as the Agent-model enrichment).
- **Scoring:** both moves start "unscored" under §4b. A plausible mechanical
  success oracle for confessed-stopgap (marker removed, or blocker text added
  near it, within the ~10-event window) is sleep-pass-1 design work, not build
  work — don't guess it in.
- **Honest residue:** the competent unconfessed shallow fix — a right-looking
  patch at the wrong level, no markers, no suppression narration — is
  invisible to a window classifier at any tier. That class stays with
  `coaching/enumerate-levels`, the CLAUDE.md hard rule at the narrative layer,
  and code review. This section narrows the funnel; it does not close it.

## 2c-ask. Shortcut-fork guard on AskUserQuestion (built 2026-07-04, Sonnet)

Same "fix at the root, not the symptom" goal as §2c, but for a different
failure shape: the model doesn't write a stopgap, it asks the user for
*permission* to write one — offering the cheap option as "(Recommended)"
alongside the real fix. Incident (2026-07-04, session
`d8f20679-e15b-42de-a7a8-ac85aa28a68a`, labeled in `eval/labels.jsonl`): an
orchestrator hit a "cheap approximation vs. proper transform primitive" fork
for the D17-juice work and asked with the shortcut marked Recommended.

Why this can't be a §1/§2 observer move: an `AskUserQuestion` wait produces
no tool events, so nothing revives an idle-exited observer during the wait,
and even a live observer's whisper is asynchronous — it can only land
*after* the question has already rendered and the session is already
blocked on the user. The moment that needs to intervene is the one right
before the popup exists, which means synchronous, which means PreToolUse,
which means deterministic (no classifier call in the critical path).

**Build:** `common.detect_shortcut_fork(tool_input)` — a second vocabulary
table, distinct from `STOPGAP_MARKERS` (that one screens code-diff content;
this one screens an `AskUserQuestion` call's own option text). Fires when,
within one question, the option labeled `(Recommended)` matches
`SHORTCUT_WORDS_RE` (approximate, minimal, stopgap, for now, quick, defer,
later, "without a new …") and a *different* option in the same question
matches `ROOT_FIX_WORDS_RE` (proper, full, real, fundamental, redesign, "new
primitive", root cause). `.claude/hooks/ask-question-guard.py` wires this as
a `PreToolUse` hook matched on `AskUserQuestion`: on a hit it denies once
with a reason quoting fix-at-the-root, bounces via a hash-keyed marker file
(an identical re-ask passes through — never bounce the same question
twice), and also calls `valve.ensure_observer` since a question-wait is
otherwise a blind spot for the idle-exit revival path. Fails open on any
error, same invariant as every other daemon component.

**Scope, honestly:** this is a keyword-pair regex, not a classifier — it
only catches the incident's *shape* (recommended-cheap vs. named-proper,
paired in one question), not every semantic variant of the same failure. A
shortcut-fork question with no matching keyword pair, or no `(Recommended)`
marker at all, passes through unguarded. Whether a classifier-side move is
worth adding for that residue is sleep-pass-1 work, noted in the label.

## 2d. Phase-transition tier (SPECCED 2026-07-05, Fable, during sleep pass 1 — Sonnet builds shadow mode; pass 2 gates delivery)

**Why.** Pass-1 grades: the three coaching fires of the week went 3/3 TP, but
coaching moves almost never fire — the rubric's evidence contract (verbatim
quote, confidence ≥0.8, observable markers only) structurally excludes
situations whose evidence is an ABSENCE: no investigation ever happened, no
verification ever ran. You cannot quote an absence. Meanwhile the classifier
already emits a `phase` judgment on every window and nothing consumes the
sequence. This tier makes that dormant stream load-bearing: deterministic
rules over phase *transitions* fire predictively — before the damage, not
after the artifact — and each rule carries a natural scoring oracle, which
kills the "predictive moves are ungradeable" objection.

**Mechanism.** The observer keeps a per-mailbox `phase_history` — (window
end_event_count, phase) appended at each verdict in `_handle_window`,
persisted in the session firestate so revives don't amnesia it. Rules run
after each append and fire through `_resolve_fire` like every mechanical
move: same cooldowns, same one-whisper invariant, same mailbox routing.
Zero new classifier calls; the tier is free.

**Initial rules (each a `phase/*` move — note: `validate_move_id` and the
catalog parser must learn the new family):**
1. `phase/implementing-without-investigating` — TASK is diagnosis-shaped
   (cheap regex: fix|bug|broken|why|crash|wrong) AND phase enters
   `implementing` with zero `investigating` windows since TASK was set.
   The "building before you've looked" moment, caught before the wrong fix.
2. `phase/no-verify-before-reporting` — phase enters `reporting` with zero
   `verifying` windows since the last `implementing` window. Fires on the
   transition, before the done-claim text exists (verify-claim's blind spot).
3. `phase/stuck-oscillation` — implementing↔stuck alternates ≥3 flips inside
   a short window span. This is coaching/differential's moment, detected
   structurally; reuse that payload rather than authoring a new one.

**Scoring oracle.** Mechanical, per fire: did the missing phase appear within
K=6 windows after delivery? Appeared → success; never → failure; session
ended first → unscored. Extends §4b's family table — unlike the coaching
moves, these are NOT permanently unscored, so auto-mute and utility gating
apply from day one.

**Shadow mode first — the gate.** Ship the rules logging `phase_fire`
telemetry WITHOUT delivering anything for the first week. Pass 2 hand-grades
the shadow fires (they double as the payload-authoring specimens, satisfying
the ≥1-specimen rule) and flips delivery on per-rule at ≥60% shadow
precision. Costs nothing to run; burns no trust while untuned.

**What NOT to build:** no task-state model, no goal tracking, no extra
classifier calls — the phase stream plus a TASK-shape regex is the whole
input. Known risks, all measured by shadow mode before delivery: phase
labels have never been graded; cadence windows make transitions coarse; the
TASK regex is crude. If shadow shows the phase labels themselves are noisy,
fix the rubric's phase definitions first — do not tune rules on top of a
broken signal.

## 2e. Advice tier — framing + recurrence (Peter, 2026-07-05; built same conversation, Fable)

The priming-tier moves (reasoning-primer, design-primer) are scheduled
general advice, not detections — but they shipped wearing the alert wrapper,
which has two costs. First, framing: a model receiving an alert it knows it
didn't trigger learns "daemon notes are wallpaper", and that alert-blindness
bleeds into the anchors that DO mean something — the tiers share one
credibility budget. Second, cadence: once-per-session means the advice has
scrolled far out of effective context long before a long orchestration run
ends, exactly the sessions that need the prior most.

Two changes, both keyed off a new `kind: advice` field in moves.md
(parse_moves default: "alert"):

- **Framing.** `valve.build_block` wraps advice moves in a distinct
  `<daemon-advice>` tag with a frozen preamble stating explicitly: scheduled,
  not a detection, nothing is wrong, nothing to acknowledge or grade. No
  supervised-mode ack sentence, no habit-memory ordinal — both read as
  accusation under an advice frame. Alert moves are untouched; the distinct
  tag (not an attribute) is deliberate, so skimming models and RUNBOOK greps
  can't conflate the tiers.
- **Recurrence.** New cooldown class `advice-recur` (COOLDOWN_EVENTS: 300
  tool events) replaces "once": first fire unchanged (first live tool event /
  first design-doc edit), then the move re-arms per target — main session
  and each worker — so a 1500-event orchestration gets the prior refreshed
  roughly five times. `last_fire_event` is firestate-persisted for the main
  session, so idle-exit revives don't double-fire; worker gates are
  in-memory like the rest of worker state (known, accepted). Advice moves
  never escalate to escalate/checkpoint — recurrence is the schedule
  working, not habituation — and are exempt from session self-grades: the
  sleep pass grades them from downstream behavior only (RUNBOOK step 2).

Grading note for passes: an advice fire's `correct` is trivially TP (the
trigger is mechanical), so precision denominators should exclude the advice
tier or report it separately — its only meaningful metric is pass-judged
effectiveness, and `effective: unclear` is the expected common case, not a
defect.

## 2f. Session-fact store (SPECCED 2026-07-05, Fable — Sonnet builds; Peter approved direction same conversation)

The dominant FP class across sleep pass 1 and live fires since is observation,
not wording: the window can't see something the session already established.
Specimens: scope-drift 1/11 (attribution blindness — the context switch that
explained the "drift" was ordered by the user or a hook, earlier than RECENT
reaches); several verify-claim FPs where the verifying event existed but sat
beyond the window ledger; the 2026-07-05 live FP in session `65084a7d` (claim
"wrapper live immediately" flagged; its verifying event — a fresh-process
build_block render — ran ~10 events before the claim, outside the window that
held the summary text). Signature rewording cannot fix this; rounds 1–2
proved wording tops out while the binding constraint is what the classifier
can see.

**Build:** the observer already tails every event — it extracts durable facts
deterministically as they pass and carries them forward past the window
horizon. A `SessionFacts` struct per mailbox (main + workers), persisted in
the existing firestate file (main; workers stay in-memory like the rest of
worker state):
- `last_verification` per class — test-run / lint / render-read (Read of
  *.png) / script-run — as (event_count, one-line label). Classes detected
  from tool name + input by regex, same tier as STOPGAP_MARKERS.
- `task_addressed` — already exists as a bit; fold it in unchanged.
- `context_switches` — user- or hook-ordered task changes with event counts
  (the scope-drift attribution fix: a TASK that changed because the human
  changed it is not drift).
- `paths` — the existing unread-edit `paths_seen` set, extended with last
  Read / last Edit event counts per path.

Rendering: a `SESSION FACTS:` block appended to every classifier window
(WINDOW_VERSION bump), e.g. "last test run: event 412, 9 events ago
(cargo test -p ...); TASK set by user at event 388; file X last read event
301". The rubric's existing exemptions ("never flag claims whose verifying
event is in the LEDGER") extend naturally — the facts block is a longer
ledger, not a new contract. No new classifier calls, no new moves.

**Scoring oracle:** verify-claim and scope-drift FP rates before/after, from
normal pass grading; the scope-drift mute expiry (~2026-07-12) is the natural
re-test — unmute into the facts-block world and grade a week.

**Honest scope:** facts are regex-extracted, so semantic verification ("the
test that ran actually covers the claim") stays with the classifier; the
store only widens what it can see. Facts persist per session, never across
sessions — cross-session memory is what MEMORY.md is for.

## 2g. Workflow launch gate (SPECCED 2026-07-05, Fable — Sonnet builds; Peter's ask: auto-approve bounded workflows, no babysitting)

Peter's complaint, same conversation: workflows need manual approval, and
unbounded scripts "destroy token usage" — but he doesn't want to monitor an
orchestration session just to click approve. Same shape as §2c-ask: rare,
already-blocking, damage-preceding event → synchronous deterministic
PreToolUse gate, this time matched on the `Workflow` tool, and this time the
common case is ALLOW.

**The bounds reframe (the design's core):** the hook never predicts true
token cost — it verifies the script is *structurally bounded* and the bound
fits an *allowance*. "How much may run without asking me" is one number
Peter sets like a card limit, not a prediction problem. Runtime enforcement
(the Workflow budget ceiling, when a token target is present) does the real
limiting; static analysis only has to confirm a guard exists.

**Build:** `.claude/hooks/workflow-gate.py`, PreToolUse on `Workflow`;
config `.claude/daemon/workflow-bounds.json` (gitignored-adjacent runtime
config, committed default): `{max_auto_agents: 10, max_auto_budget_tokens:
100000, require_estimate: true}` — placeholders only; Peter rejected an
initial 30-agent draft as way too high (2026-07-05), final numbers are his
call via the design-doc-systems discussion. Open bounds questions for that
discussion: per-workflow vs per-session cumulative allowance (chained
auto-approved workflows multiply exposure), and whether worker model tier
scales the allowance (10 Sonnet agents ≠ 10 Opus agents in cost). Static
checks over the script text (regex tier, no AST dependency):
1. Count literal `agent(` call sites; find `parallel(`/`pipeline(` over
   array literals (fan-out = literal length) vs. over variables (unknown —
   treat as unbounded unless a `.slice(0, N)`/length guard is visible).
2. `while`/`for` loops whose body contains `agent(`: bounded only if the
   condition references `budget.remaining(` or a literal counter cap.
3. `meta.description` must state an agent estimate (`/\b\d+\s*agents?\b/`)
   when `require_estimate` is on — the approval line shows a number.
Decision: every check bounded AND estimate ≤ max_auto_agents →
`permissionDecision: allow` with a one-line note ("workflow-gate:
auto-approved, ≤N agents, bounded loops"). Anything unbounded, over
allowance, or unparseable → `ask` (today's behavior — the gate never
hard-denies and fails open to ask), with the specific reason attached so a
manual approval is informed.

**Probe first (step 0, like §2b's):** whether the runtime budget ceiling
(`budget.total`) can be armed by the orchestrator's own turn rather than
only by user "+Nk" text. If yes, the gate should require an armed budget
for auto-approve, making the allowance a hard runtime wall, not a static
guess. If no, static structural bounds carry the gate alone — say so in the
config comment.

**Telemetry:** `workflow_gate` records (decision, reasons, estimate) →
sleep passes review allowance fit; two weeks of all-ask or all-allow means
the numbers are wrong.

**Launch tier — BUILT 2026-07-05 (Fable, at Peter's direction; separate
from and prior to the bounds tier above, which remains Sonnet's ticket).**
The incident: Peter caught an Opus orchestrator launching a workflow whose
`agent()` calls carried no `model:` — every worker silently inherited the
session's Opus tier and reused the session defaults. The Agent-tool version
of this failure has an async anchor (`anchor/agent-model-discipline`); the
Workflow script path had none, and a whisper after launch is too late — the
fleet has already spent the tokens. So: `.claude/hooks/workflow-gate.py`,
PreToolUse on `Workflow`, two deterministic checks, no classifier call,
fail-open, `workflow_gate` telemetry on every decision:

1. **Model discipline, every launch.** Every `agent(` call site in the
   script (string-stripped, paren-balanced scan; `scriptPath` files are
   read) must carry an explicit `model:`. Inheriting is never a choice —
   it resolves to the orchestrator's own tier. Violations deny with the
   offending line numbers and are re-checked on every retry; this tier
   cannot be waited out.
2. **Announce-once, per (session, workflow name).** The first launch of a
   given workflow is denied once with pre-authored instructions (payload
   text in the hook, sleep-pass-editable like moves.md): announce in
   visible text what the workflow is for, why it needs orchestration over
   inline work, the fan-out, and the model tier of every stage with a
   reason — reasoned before launch, not restated defaults. The deny embeds
   the parsed roster so the announcement is grounded. Keyed on the meta
   `name`, NOT a content hash — the retry usually edits the script to add
   `model:`, and a content key would bounce the fixed script twice.

A launch clearing both tiers emits NO permission decision — it falls
through to today's manual approval, with Peter reading the announcement
above the prompt. This tier only adds requirements; only the bounds tier
may ever auto-ALLOW, and when it lands it slots in behind these checks.
Known residue: nested `workflow()` child calls inside a script never
re-enter PreToolUse (covered only insofar as the parent script text is
parsed); name-only saved workflows can't be model-checked (announce-once
still applies). Tests: `.claude/hooks/test_workflow_gate.py` (27 checks).

## 2h. Final-window extensions (SPECCED + BUILT 2026-07-07, Fable authoring + two Sonnet builders, same day — all sections shipped; workflow-agent DELIVERY unproven pending the live probe, observation tested against real layouts)

Context: Peter's two directives from the final-window session. (1) "The daemon
must fire BEFORE my next message — otherwise it's pointless." Telemetry since
stop-wait v2: 52 corrective fires in-turn vs 16 a prompt late; the largest late
class is text-only turns (ungrounded-resolution ×7 of the 16). (2) 100% worker
coverage — observed, whispered, self-grading, end-of-session reviewing.
Verified gaps 2026-07-07: workflow agents entirely unobserved (transcripts one
directory below the scan); zero worker self-grade records ever; the Stop-hook
grade backstop and observation prompt are explicitly main-session-only.

### 2h.1 Chat tier: `mechanical/ungrounded-chat-claim` (Stop valve)
- Lives in `daemon-stop.py` beside announced-not-started — same turn parse
  (final assistant text + turn tool-call presence). The moves.md signature IS
  the contract: implement exactly its ALL-hold conditions and never-fire list.
- Artifact extraction: slash paths under `docs/ crates/ src/ assets/ scripts/
  .claude/` or tokens ending `.rs .md .py .json .wgsl .toml`; plus ALL-CAPS
  underscore-joined tokens resolved via `os.path.exists("docs/<token>.md")`.
  Strip fenced code blocks BEFORE extraction; exclude artifacts appearing in
  the user's message this turn.
- Grounding scan: one pass over the session transcript's tool_use events
  collecting INPUT strings (file_path / command / pattern arguments); an
  artifact is grounded iff some earlier tool call's input contains it. Tool
  OUTPUTS never ground anything — a mention inside a read's output is the
  stale-memory failure this move exists to catch.
- Delivery priority at Stop: existing pending whisper > announced-not-started
  > this. One-whisper invariant intact. Applies to main session and (behind
  the worker flag) worker Stop events alike.
- Scoring oracle for pass 2: within ~10 events of delivery, either a grounding
  event for the named artifact appears (success) or the next assistant text
  re-marks the claim unverified (success); neither = fail.

### 2h.2 `mechanical/landing-doc-reflex` (observer Bash tier)
- Lives beside `_check_stopgap` in `observer.py` — live-tailed Bash events
  only, never catchup. Trigger per the moves.md signature; PORT the
  merge/push-to-main command matching from `preToolUseBash.py`'s
  landing-protocol guard (read the mechanism, don't re-derive it).
- Docs-only suppression, honest approximation (the observer runs no git): use
  §2f's per-path edit facts — suppress when every Edit/Write in the trailing
  window touched only `docs/`, memory, or `.claude/` paths. Note the
  approximation in code.
- Standard cooldown covers the merge+push pair (one fire per landing).
- Scoring oracle: a Status-line edit to a `*_DESIGN.md` and/or a
  `docs/landings/` write appears in the ledger before the push completes or
  within ~10 events after the fire.

### 2h.3 Workflow-agent observation (extends §2b)
- `_scan_agents` additionally lists `subagents/workflows/*/` for
  `agent-*.jsonl` (workflow run dirs appear mid-session; one extra directory
  level). agent_id from filename as today — ids are unique hex, collision-free
  across runs. Mailbox key `<session>.<agent_id>` and valve routing unchanged.
- MANDATORY step-0 probe before wiring delivery (same method as §2b's
  2026-07-04 probe, temporary instrumentation + one trivial 1-agent workflow):
  confirm (a) PostToolUse fires for workflow-agent tool calls with agent_id
  set, (b) additionalContext lands in the workflow agent's own context. (a)
  holds but (b) fails → ship observation+telemetry only, record the residue
  here. (a) fails → transcript observation still works, delivery impossible,
  record. The worker-nudges flag gates delivery exactly as for Agent workers.

### 2h.4 Worker self-grade + end-of-session review (Stop valve, workers)
- Seam: `daemon-stop.py` already receives worker Stop events (agent_id set).
  Behind the worker-nudges flag, add worker-scoped grade backstop +
  observation prompt: at most once per (session, agent_id), own sentinels;
  activity threshold ≥20 tool events (workers run shorter than the main
  session — 40 would exempt most real workers); grade backstop only when
  telemetry shows that agent_id actually received delivered gradeable fires.
- Worker ack: verify `valve.py`'s supervised ack + "this fire: seq N" line
  reaches worker deliveries too; parameterize if main-only. Worker grade
  lines carry `agent_id` per RUNBOOK step 2.
- Non-negotiable escape valve (the observation-prompt lesson, 2026-07-05):
  reason text says nothing-to-add is fine, no filler entries, never blocks a
  clean worker more than once.
- Scoring oracle: count of agent_id-bearing grade lines rises from its
  current all-time value of zero.

### 2h.5 Hygiene (fold into whichever build touches verdicts/)
- Startup sweep in the observer: delete `verdicts/` sentinels older than 7
  days (`.stopblock.*`, orphan `.stop`, any pre-v2 residue). Conservative:
  age-keyed only, never touch live pidfiles/mailboxes/firestate.

### 2h.6 Late-fire forensics + the three delivery fixes (added 2026-07-07 after the diagnosis run)
All 17 next-prompt fires since stop-wait v2 were classified against the
transcript's `stop_hook_summary.hookInfos[].durationMs` field — the measured
Stop-hook wall time, a direct oracle for whether the wait engaged (record
this oracle; pass 2 should use it instead of inference). Split: 11 TEXT-ONLY-
RACE (wait engaged, ran to the 6.0s cap, durations clustered 6130–6250ms —
genuine classifier latency; 8 of the 11 are done-claim family, structurally
turn-final), 5 VERDICT-AFTER-TURN (durationMs 27–44ms with the flagged text
landing 27–77ms before Stop: the hook's transcript-size snapshot raced the
final text's write, read a stale size, concluded drained >= target, and
skipped the wait with the observer provably alive — a DEFECT), 1 anomaly
(110-min transcript stall, unexplained, logged only). Zero pre-v2, zero
dead-observer fail-opens, zero mid-turn misses. Fixes, all in daemon-stop.py:
- **(a) Snapshot race fix:** stat the transcript twice ~200ms apart at Stop,
  target = the max (re-stat once more if still growing). Costs ~0.2s per
  turn end; converts the 5 VERDICT-AFTER-TURN fires. Root fix, not tuning.
- **(b) `STOP_WAIT_CAP_S` 6.0 → 10.0.** The 11 capped waits sat exactly at
  the cap, so the classifier was close behind. Peter's 2026-07-07 metric
  ruling (corrections land before his next message, "otherwise it's
  pointless") re-prices the latency trade the 6s cap froze. Throttling
  degradation unchanged: dead/stale observer still means no wait at all.
- **(c) `mechanical/unverified-done-claim`** per its moves.md signature —
  the zero-latency tier for the done-claim family (8/11 of the capped
  class). Shares 2h.1's turn parse; verification-class detection imported
  from common.py. Delivery priority at Stop: pending whisper >
  announced-not-started > ungrounded-chat-claim > this. Scoring oracle: a
  verification-class event within ~10 events of delivery, or the claim
  restated as unverified; UNVALIDATED, pull if pass 2 grades it noisy.

## 3. Payload library (`moves.md`)

Two families, one format. **Coaching moves** fire on phase transitions (hypothesis
formed, fix chosen, stuck, circling) and inject a reasoning move. **Drift anchors**
fire on degradation tells (claim without verification, hedging creep, scope drift,
thrash) and re-anchor. Each entry: id, situation signature (what the rubric looks
for), exact payload text, cooldown class. The rubric references moves by id; the
valve resolves id → text. Haiku never composes prose that reaches the working
model — it only selects.

## 4. Replay harness (`replay.py`)

The tuning loop, and the reason this can be built safely. Historical session
JSONLs exist on disk (~100+ under `~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/`),
and the feedback memory corpus is labeled incident data — each `feedback_*.md`
documents a real failure in a real session. Replay runs the observer's windowing +
classifier over historical transcripts offline and reports what would have fired.

**Go-live gates** (measured on replay, before any live injection):
- Known-incident recall: the classifier flags the drift in ≥ 60% of sessions with
  a documented failure, *before* the point where the user intervened.
- Quiet-session noise: < 1 false flag per clean session on average.
- Precision on a hand-checked sample of firings ≥ 80%.

A wrong whisper erodes trust in all whispers; when precision and recall trade off,
buy precision.

**Amendment 2026-07-03 (Peter's call, Fable concurring):** the offline replay
gate is waived in favor of a supervised live rollout. Replay rounds 1–2 ran
honestly (recall 8%→42%, noise 2.0/clean session — commit `790e0d58` has the
diagnosis); the remaining fixes are structural and are now folded into this
spec (assistant-text cadence, `task_addressed` state, verify-claim reword,
move-id validation). Rather than paying for a third offline run, the system
goes live in **supervised mode**: whispers are injected but carry an explicit
`unvalidated` marker in the `<daemon>` tag (marker REMOVED 2026-07-04 with the
confidence attribute — Peter judged both licenses to discount the anchor;
supervision continues via the acknowledgment sentence) and are surfaced to Peter, every
injection is logged, and §4b outcome scoring + auto-mute are active from day
one. The gates above still stand — they are now scored from live telemetry by
the sleep pass instead of from replay. `replay.py` stays as the offline
scoring harness for future rubric changes.

## 4b. Live outcome scoring + auto-mute

The daemon scores each injection mechanically, in-session, and appends the score
to its telemetry record: did the behavior the payload asks for appear within the
next ~10 tool events (verify-claim → a test/run/render; thrash → the error streak
ended; circling → a different tool class used)? Not every move has a mechanical
outcome — score the ones that do, mark the rest `unscored`.

Auto-tuning is split by risk:
- **Auto-adjustable (bounded numeric dials):** per-move cooldowns and confidence
  thresholds, within floors/ceilings set here — never below 0.7 confidence, never
  under 10-event cooldown.
- **Auto-mute:** a move that fires ≥ 5 times with a 0 scored-success rate is
  disabled for the remainder of the week and queued for the sleep pass. Degrade
  toward silence, consistent with fail-open.
- **Never automatic:** payload or signature *wording*. Text edits happen only in
  sleep passes (big model, committed, diffable). A loop that rewrites its own
  words on its own measurements drifts unsupervised.

## 4c. Approved extensions (Peter, 2026-07-04 — from the external-review pass; build after §4b)

1. **Richer windows** (Sonnet, can ship immediately): WindowState grows arithmetic
   the classifier can't do reliably — repeat-target counts annotated on ledger
   lines ("(4th touch this session)"), consecutive-failure streaks, and events
   since TASK set. No rubric/signature edits needed. Every verdict record gains a
   `window_version` int so §4b/sleep-pass scoring never mixes regimes.
2. **Habit memory** (Sonnet, after §4b): at observer start, roll up telemetry
   fires per move over the trailing 7 days across sessions; when a move fires,
   the valve appends a FIXED-FORMAT line to the payload: "(Nth fire of this move
   across sessions this week.)" Amendment to invariant 5: a mechanically
   computed count inside a fixed template is not a wording edit; the template
   itself is sleep-pass-only.
3. **Utility gating** (fold into the §4b build): once a move has ≥10 scored
   fires, its confidence threshold scales inversely with scored success rate,
   clamped to [0.7, 0.95]. Bounded numeric dial per §4b; no new machinery.
3b. **Fatigue ordinal** (small follow-up to the shipped §4b code): every scored
   injection records its fire ordinal within the session (nth fire of that
   move). Sleep pass tests whether scored success decays with ordinal:
   decay = payload fatigue (recognizable has become ignorable) → wording
   tiers go on the sleep-pass agenda; flat = fixed wording vindicated.
4. **Falsification experiment** (design in sleep pass 2, run after the
   supervised week): alternate sessions between the daemon and a control arm
   injecting one generic reminder every N tool events with no classifier; score
   both arms with the same §4b telemetry. Pre-committed: if the control
   matches, the classifier is decoration and the thesis reduces to "inject
   more often." Third arm (added 2026-07-04): the daemon's timing with the
   note replaced by a content-free marker — separates interruption from
   content (any injection perturbs; this measures the interruption floor).
5. **Retrieval note (not scheduled):** at ~100x incident corpus (multi-repo,
   multi-user), the named-move library stops being the right compression — an
   embedding space over historical episodes replaces id-selection. Below that
   scale, do not build it; it trades away auditability and habituation for
   coverage the data can't pay for.

The paper (`paper.html` v2, §10) is the readable version of this list.

## 5. Sleep pass (consolidation)

Weekly scheduled routine on the largest available model. Input: `telemetry.jsonl`
joined with the transcripts it points into, plus session digests. The question for
every injection: did behavior change after the whisper — different action, claim
verified, circling stopped? Output: revisions to `rubric.md` and `moves.md`
(committed, so drift of the daemon itself is diffable), retirement of moves that
fire without effect, new tells from failures the rubric missed. This loop is what
keeps the daemon learning after its author is gone; without it the system
fossilizes into a museum of 2026 failure modes.

## Invariants

1. **Fail open, always.** Daemon dead, verdict stale (> 5 min), file unparseable →
   valves do nothing. The daemon is additive-only; it must never be able to
   block or slow the session. No exceptions — a safety layer that can break the
   show is worse than none.
2. **Haiku detects, never prescribes.** Nothing Haiku writes is ever shown to the
   working model. Selection from fixed payloads only.
3. **One whisper at a time.** Single live flag, cooldowns enforced. Ten units per
   turn is the book again; the model will skim it like it skims CLAUDE.md.
4. **Every injection is logged.** Unlogged steering is untunable steering.
5. **Payloads and rubric are versioned; edits happen in sleep passes or by hand,**
   never by the daemon at runtime.

## Build order (next sessions, any model — updated 2026-07-03 per §4 amendment)

1. `replay.py` windowing + classifier call — DONE (rounds 1–2 run; harness kept
   for offline scoring of future rubric changes).
2. Structural fixes in the shared windowing code (`common.py`), per §1 as
   amended: analyze on every assistant text event; `task_addressed` window
   state; validate move ids against the catalog. (The verify-claim signature
   reword is already in `moves.md`.)
3. `observer.py` — wrap the same windowing in the tail/cadence/verdict loop.
4. Valve hooks + `settings.json` wiring, with the fail-open checks. Whispers
   carry the `unvalidated` marker (removed 2026-07-04, see §4) until the sleep pass clears the gates from
   live telemetry.
5. Supervised live period: injections logged AND surfaced to Peter; §4b
   scoring + auto-mute active.
6. Schedule the sleep pass; hand the first two to Fable (budget reserved).
   First sleep pass also owes: score the §4 gates from telemetry, and author a
   new move for the `735b0dc6` family (confident wrong system description,
   never grounded in code) — Fable-only work.