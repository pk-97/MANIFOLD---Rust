# Agent Routing — task shape → model, profile, gate

**Status:** ACTIVE 2026-07-19 · steering model added 2026-07-20 (Peter's directive after the overnight-wave autopsy; plan: `docs/SYSTEM_UPGRADE_2026_07_PLAN.md`) · Opus middle-orchestrator added 2026-07-21 (Peter's directive: Fable window economy). Authoritative staffing/routing policy. CLAUDE.md §Agents and the `agent-model-staffing-preferences` memory are pointers here.

## The steering model (2026-07-20 — supersedes review-at-landing-only)

The overnight waves failed because Sonnet orchestrated Sonnet and let 100% of green through. Rules now:

- **A judgment-tier model (Fable, or K3 as top session) is the only orchestrator.** Never Sonnet-over-Sonnet, at any depth.
- **The orchestrator steers, not just reviews.** It chooses the approach before the lane spawns: every brief names the existing system the work rides on (the *reuse target*) and the conviction test that must fail before the fix. Building a parallel path is a brief violation, not a judgment call.
- **Lanes make exactly ONE commit, then STOP and report.** The orchestrator reviews that first commit before the lane continues — wrong direction always shows in the first diff.
- **Lanes have NO landing rights.** Only the top session merges to main. Lane branches are safe-to-abandon.
- **Decisions flow up.** "Existing system doesn't cover X" or "this needs a new helper module" = stop and report, never improvise.
- **Review is the throttle.** Up to 8 lanes, but diffs queue for orchestrator review; landing never outpaces review.
- **Per-wave adversarial pass.** Before a wave spawns, a Fable fork attacks the brief set (wrong fix shapes, non-disjoint lanes, over-deletion risk).
- **Resume-note in every brief.** If the top session dies: lane state = branch + findings doc, recoverable by the next session.
- **Opus middle-orchestrator for multi-stage waves (2026-07-21, Peter; PROVEN same day across 3 rotations, workstreams A/B/C):** Fable usage is extremely expensive. Mechanical work is ALWAYS a Sonnet agent. When the work has multiple stages or waves to sequence, Fable spawns ONE Opus orchestration agent to manage the Sonnet workers and reports up; Fable keeps final authority — approvals, escalations, doctrine, and post-landing spot-checks. Single-stage mechanical work needs no middle-man: one Sonnet agent direct. This is an orchestration seat, not a lane — "no Opus lanes" stands unchanged. The proven contract:
  - **The Opus seat owns the landing ceremony** (GIT_TREE_DISCIPLINE §2 end-to-end, including the full-workspace gate, bug_status reflow in the worktree, and push). It sends Fable a POST-landing summary quoting the gate lines. It never force-pushes or `branch -f`s. Fable spot-checks landings after the fact (`git log` + ancestry), not before.
  - **Escalate-only-on:** non-trivial merge conflicts, red gates it can't attribute, product/UX judgment, anything wanting new shared state or a new public API. Everything else it handles.
  - **Rotate per WORKSTREAM, not per token count.** One Opus orchestrator per workstream; at the boundary it writes a handoff file (scratchpad) — state, anchors, briefs-in-flight, doctrine observations — and stands down; Fable spawns the successor seeded with the file. A compacted orchestrator reviews worst exactly when it matters most (the landing).
  - **Lanes run FOREGROUND (blocking Agent calls), not background.** Background-lane completions route to the top session, making Fable a mandatory relay — the single biggest residual Fable leak of the proving run.
  - **Two load-bearing orchestrator duties (each caught a real error on the proving run):** (1) reproduce/instrument a suspected root cause — including a lane's own STOP-diagnosis — before briefing or accepting it (caught BUG-296's wrong backlog root and the VD-035 phantom "layout bug"); (2) verify lane claims itself before landing — rerun the named tests/flows, review the diff against the brief's stated constraints INCLUDING comment-truth (caught a machine-fragile float assert and a stale comment asserting the pre-fix world). Without these two duties the seat is an expensive message-forwarder.
  - **Briefs restate the invariants every time** (one commit then stop, pathspec commits, no landing, worktree via the slot ring, explicit `model`, LOW effort). The proving run held discipline because every brief repeated it, not because agents absorbed CLAUDE.md.
  - **Enforcement is mechanical where possible:** `agent-model-guard.py` (explicit model per spawn), `agent-tier-spawn-guard.py` (denies Agent spawns by Sonnet/Haiku-tier callers — kills Sonnet-over-Sonnet at any depth by machinery, not policy), `agent-worktree-isolation-guard.py` (no ad-hoc worktrees).
  - Heartbeat: for long orchestration waits the top session keeps a ~50-min ScheduleWakeup liveness check (also keeps its prompt cache warm); orchestrators do NOT self-ping.
  - **The Opus seat MAY implement directly (Peter's ruling 2026-07-21, after the WS3 queryable-rows landing):** when the work is delicate, design-integrated, or needs empirical iteration (run-observe-adjust loops, selector/storage semantics), the orchestrator writes the code itself in its worktree instead of briefing a Sonnet lane — the lane round-trip plus re-verification would cost more than direct work. Sonnet lanes remain the route for mechanical bulk on fully-decided briefs. This is the seat implementing, not an Opus lane: one seat per workstream, same landing ceremony, same escalate-only-on list. Flag the call in the post-landing summary.

## The tiering

| Seat | Model | Role |
|---|---|---|
| Lead intelligence | **Fable** | Design, judgment, review, verification, landing. Owns every decision and every landed diff. |
| Consult peer | **Kimi K3** (via cc-fleet) | Second strong opinion at named moments only. Expensive — never a lane worker, never routine. |
| Mechanical executor | **Sonnet 5 / K2.7** (`kimi-for-coding-highspeed`) | Bulk implementation on fully-decided briefs. Never asked to design or judge. |

K3 is a Fable-level model priced like one. The earlier "K3 = default lane agent" routing is dead; so is "K3 orchestrates Sonnet lanes" as a standing configuration — when Fable is the session, Fable leads and K3 is consulted, not staffed.

**When K3 IS the top-level session** (Peter opened the session on K3 as the main orchestration agent — no Fable above it), the consult-only rule doesn't apply: K3 leads by default. It designs, implements, verifies, and LANDS its own work under the same landing protocol and the same verification bar this doc sets for Fable (§Verification: adversarial pass, citations checked, gate rerun on the merged tree). What it still doesn't do is spawn K3 lane workers under itself — lanes under a K3 session are Sonnet/K2.7 mechanical executors. (Peter's directive 2026-07-19, same session as this doc.)

## When K3 is consulted (the only two triggers)

1. **Design fork** — during design, when Fable has a genuine fork the audit can't kill (the §5 alternative-killing step in DESIGN_AUTHORING.md). One focused question, not an open-ended review.
2. **Pre-dispatch sanity check** — before sending a *large* mechanical wave (multi-agent bulk work), K3 reviews the brief set for wrong fix shapes, missed blast radius, scope creep. Ordinary single lanes skip this.

Consult output is advice; Fable integrates and owns the call. Spawn: `cc-fleet subagent kimi-code --prompt-file <brief> --profile slim-ro --background`.

## What mechanical agents get

Task shapes that route to Sonnet/K2.7: mechanical sweeps, clippy/format fixes, test runs + log reading, doc regeneration, read-only surveys with named targets, implementation where the fix shape is already written down in the brief.

Never to mechanical agents: graph semantics, GPU/kernel work, undo/lifecycle, design judgment, anything where the fix shape isn't already decided.

**Reasoning effort (2026-07-20):** mechanical lanes run at LOW effort — a fully-decided brief leaves nothing to deliberate, and overthinking is how executors "improve" the brief into parallel infra. Not zero: conflicts and gate failures still need a little reasoning. Investigation/consult work keeps normal effort.

## The brief contract (where the tokens are saved)

Slow flows and bug residue come from agents re-deriving what the lead already knows. Every lane brief carries:

- **Established findings** with file:line anchors — never send an agent exploring for what a memory, backlog entry, or the lead's own audit already records.
- **Exact scope** — the files it may touch; write access only after scope is agreed (read-only profile for investigations).
- **The gate command** it must run and what "done" means in writing.
- **Pre-allocated BUG-id range** if parallel lanes may log bugs.

## Verification

One strong verify pass per lane before landing: adversarial review ("refute this diff against the brief and the gate"), citations checked, gate rerun by the lead. Two weak passes don't sum to a strong one — cheap-agent-reviews-cheap-agent is how plausible-looking drift lands. Small lanes, frequent landing: 2–3 commits per phase beats one hours-long wave.

## Provider facts (cc-fleet / Kimi)

Spawn: `cc-fleet subagent kimi-code --prompt-file <brief> [--profile slim-ro] --background`; resume with `--resume <session-id>` (keep profile constant across turns). Provider `kimi-code`, endpoint `api.kimi.com/coding/`, flat Allegretto membership window. Gotcha: `kimi-for-coding` on the endpoint is K2.7, NOT K3. Cost reality (measured 2026-07-18): Kimi bills cache reads ~$0.80/MTok and cache reads are ~90% of lane volume, so K3 is only "cheap" against the flat window — per-token it costs more than Sonnet list. That's the pricing basis for K3-as-consult.

No Opus lanes anywhere (overthinks, rabbit-holes — Peter's settled call). All agents obey every rule in CLAUDE.md — worktree slots, pathspec commits, the landing protocol.

Related: `agent-execution-playbook` memory (hazards), `docs/DESIGN_AUTHORING.md` (upstream of routing — how work gets shaped), `opus-prompt-pack` memory (paste-ready prompts).

## Overnight orchestration pattern (added 2026-07-21, god-file wave — Peter's directives)

Proven shape for unattended multi-slice runs; use it whenever a phase's judgment is DONE and only mechanical bulk remains.

- **Three tiers, strict:** Fable top session = judgment + landings only, alive on ~50-min wakeups. ONE Opus dispatcher seat = clerical: pop the slice queue, brief a Sonnet lane from the template, run the exit-code gates, accept/reject, next. It holds no code in context, implements nothing, decides nothing, never lands. Sonnet lanes (LOW effort, foreground) = all code, one slice = one lane = one commit.
- **Every seat and lane is an Agent-tool TEAMMATE (Peter, 2026-07-22 — hard rule, verbatim "YOU MUST ALWAYS USE TEAMS"):** named agents spawned via the Agent tool, visible in the team panel, messaging the top session via SendMessage. Never claude-pane/tmux sessions for seats — tmux panes exist only when Peter explicitly asks for a watchable one. Teammate messages wake the top session directly; flag files remain the durable record per the decisions-are-files rule.
- **Decisions are files, not messages.** Rulings live in `.claude/orchestration/decisions.md` (append-only, team-lead writes); the queue in `ws1-queue.md`-style files; parked items in `parked.md`. Chat messages cross agent loops mid-flight (observed twice 2026-07-21) — a seat re-reads the decisions file BEFORE pausing on any fork.
- **Design rulings outlive the night (Peter, 2026-07-22): any mid-wave DESIGN decision (architecture, seam shape, deferral) is mirrored into the owning design doc as a numbered D-entry — same session, before the wave lands.** The orchestration bus is gitignored process-state; the design doc is the reviewable record future integrations read. A ruling that names a seam future work will touch gets an explicit seam note + a Deferred entry with its revival trigger.
- **Skip-and-park, never block:** any friction not covered by a standing decision → park the slice, continue the queue. Parked items drain at the top session's heartbeats.
- **Every gate is an exit code** (move-identity, census equality, invariant tests, focused clippy/nextest) — that is what makes the dispatcher clerical and the night unattended.
- **One heavy build machine-wide:** all full sweeps/flow runs through `.claude/scripts/with-build-lock.sh` (GUI-lockup incidents 2026-07-21).
- **Auto-mode command hygiene:** phrase lane commands so the permission classifier reads them as non-destructive (single-purpose commands; no `$()` writes or repo-path redirects; `/tmp`//dev/null` fine). A blocked command = park, never retry variants.
- **Scope fence per night, written into the queue:** only pre-decided mechanical phases run; semantic phases wait for daytime judgment.
- **A report is a trigger, never a stopping point (RT wave, 2026-07-23).** Dispatcher seats act IMMEDIATELY on a lane completion within their standing authority (fold, gate, dispatch next) — ending a turn on a status summary is the observed stall mode (three instances in one night). Sibling plumbing fact: lane completion notifications can route to the TOP session only, invisible to the dispatcher — lanes must SendMessage the dispatcher explicitly on completion, and the top session treats a dispatcher that goes idle after a lane finishes as possibly-blind, not merely slow (cheap git-state check before nudging).
- **Rotation on context growth:** seats hand off via recipe-carrying files at clean commit boundaries (~500K observed as the sensible ceiling); the lane + queue are the crash barrier — machine restarts cost only in-flight context.
