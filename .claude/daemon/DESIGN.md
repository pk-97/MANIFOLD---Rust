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
  the most recent user message that reads as a task, kept verbatim; (b) a
  compressed event ledger since the last window (tool name + target + ok/err, one
  line each); (c) the last two assistant texts verbatim; (d) a `task_addressed`
  bit carried forward across windows — set once an assistant text answers TASK,
  so a stale-but-answered TASK stops matching scope-drift after the answering
  text scrolls out of RECENT (replay round 2: `573a3584` false-fired 3 windows
  in a row for exactly this). Target ≤ 4k tokens. The task statement must always
  be present — phase and drift are only measurable as divergence from the goal.
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
- Cost sanity: classifier spend scales with agent count (~a few dollars per
  six-worker wave) — acceptable; per-agent cooldowns prevent whisper spam.

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
`unvalidated` marker in the `<daemon>` tag and are surfaced to Peter, every
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
   carry the `unvalidated` marker until the sleep pass clears the gates from
   live telemetry.
5. Supervised live period: injections logged AND surfaced to Peter; §4b
   scoring + auto-mute active.
6. Schedule the sleep pass; hand the first two to Fable (budget reserved).
   First sleep pass also owes: score the §4 gates from telemetry, and author a
   new move for the `735b0dc6` family (confident wrong system description,
   never grounded in code) — Fable-only work.