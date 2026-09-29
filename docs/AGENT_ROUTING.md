# Agent Routing — task shape → model, profile, gate

**Status:** ACTIVE · 2026-09-29 rewrite to the one live roster (Opus 5.5 + Fable review). Authoritative staffing and routing policy; CLAUDE.md section Agents points here. Roster history lives in git, not in this file.

## Launch profiles

Which model a slot name reaches depends on how the top session was launched. The agent-launch guard prints the live map on every deny (`live map: …`); trust that over this table if they disagree.

| Profile | Launch | Lead | `fable` | `opus` | `sonnet` | `haiku` |
|---|---|---|---|---|---|---|
| Anthropic | `claude` / `claude-m` | Fable 5.1 or Opus 5.5 | Fable | Opus | Sonnet 5.5 | Haiku |
| Kimi | `k3m` (via the litellm proxy) | Kimi K3 | K3 | K2.7 | K2.7 — classifier slot, never lanes | K2.7 |

Kimi-profile mechanics (proxy, slot remapping, the `k27-*` lane label): section Native provider lanes. Provider operations and the proxy runbook: `docs/PROVIDER_OPERATIONS.md`. Every seat defaults to LOW effort; raising it is a per-task call for a named hard problem.

## The tiering

| Seat | Anthropic profile | Kimi profile | Role |
|---|---|---|---|
| Lead | Fable 5.1 (hard design and direction) or Opus 5.5 | K3 | Design, judgment, briefs, review, landing. Owns every decision and every landed diff. The only seat that lands. |
| Landing seat | Sonnet 5.5 (`model: "sonnet"`) | none — the lead lands | Runs the landing protocol on a lead-approved branch. See section The landing seat. |
| Lane executor | `sonnet`, LOW effort — never Haiku lanes (Peter, 2026-07-29: stalls and needs nudges) | K2.7 (`haiku`; `opus` = same model) | Fully-decided briefs only: sweeps, gate runs, instruments, implementations whose fix shape is written down. |
| Consult | Fable fork | K3 fork | Read-and-discuss only. See section The consult seat. |
| Auto-approval classifier | harness default | K2.7 on the `sonnet` slot | Not a lane. If the Kimi cap bites: `seat_tool.py assign sonnet <model>`. |

There is no dispatcher or middle-orchestrator seat. The clerical loop (pop queue, run gates, park failures) belongs to workflow scripts, exit-code gates and hooks.

Never to lanes: graph semantics, GPU/kernel work, undo/lifecycle, design judgment, anything whose fix shape isn't decided. No judgment-tier lanes: the lead and consult are seats, never lane workers.

## The landing seat

The happy path of a landing is mechanical: fetch, merge `origin/main` into the branch, `scripts/landing_gate.py --repo <worktree path>` (agents cannot cd, and HEAD on main's checkout is the base, so the gate refuses there), `git merge --no-ff` to main with a `Closes:` trailer, push, close the beads. A cheaper model does that fine. What it can't be trusted with is judging whether a failure is simple, so it never judges:

- It lands only a branch the lead has reviewed and named in the brief.
- **Any non-zero exit, merge conflict, hook deny, or output it doesn't recognise → stop and report verbatim. No fix attempts, no retries with variations.** The lead takes it from there.
- It never force-pushes, never `branch -f`, never edits code.
- Report in the final text turn: the merge SHA, the gate summary line, the beads closed.

Kimi profile: the `sonnet` slot is the classifier, so the lead lands itself.

## The steering model

- **A judgment-tier model is the only orchestrator.** Never a lane model over lane models, at any depth.
- **The lead steers before the lane spawns.** Every brief names the existing system the work rides on (the reuse target) and the conviction test that must fail before the fix. Building a parallel path is a brief violation.
- **Lanes make exactly ONE commit, then STOP and report.** The lead reviews that first commit before the lane continues — wrong direction always shows in the first diff.
- **A lane's report is its final text turn, never a SendMessage** (hook-enforced, `lane-report-enforcer.py`). SendMessage is for mid-flight questions only.
- **Lanes have no landing rights.** Lane branches are safe to abandon.
- **Decisions flow up.** "Existing system doesn't cover X" or "this needs a new module" = stop and report, never improvise.
- **Review is the throttle.** Up to 8 lanes; landing never outpaces review.
- **Resume note in every brief.** Lane state = branch + findings doc, recoverable by the next session.
- **Work items are beads.** Briefs reference bead ids.
- **Lane health is scheduled, never Peter's job** (`lane-health-guard.py`, its docstring is the spec). Before a background lane spawn, arm a session `CronCreate` (30 min) whose prompt contains `lane-health-check`. Two consecutive idle checks with no report = stalled: message once, then stop the lane and escalate. Delete the job when no lanes run. Read-only consult forks need none.
- **A failing lane is respawned once, never taken over.** Stop it, carry what it learned into a fresh brief, respawn. A second failure means the task was shaped wrong: the lead re-shapes it (with the consult seat if stuck). The lead does not grind lane work itself.
- **Briefs restate the invariants every time:** one commit then stop, pathspec commits, no landing, worktree via the slot ring, explicit `model`, LOW effort.
- **Spawn hierarchy is hook-enforced** (`agent-tier-spawn-guard.py`, caller tier from the transcript's `message.model`): the lead may spawn anything; lane-tier callers may spawn nothing. Launch defects (missing model, bad name) are one deny that spells the corrected call (`agent-launch-guard.py`). Names are `<slot>-<descriptive-task>`.

## Lead token economy

Lead context is the scarcest resource in the rig. Reach for cheap seats before doing bulk searching and reading yourself: lane-tier agents for tool-using recon, `.claude/hooks/oneshot` for bounded mechanical asks (never review — it has no repo access and fabricates citations).

What never delegates: verification of evidence the lead acts on. A weak model's omissions are invisible in its own summary, so the lead spot-checks the underlying code or data. **Visual verification: judgment-tier seats only.** Opus 5.5 workers render and judge their own headless stills as part of their gate (Peter re-approved 2026-09-29: "Opus 5.5 has strong visual skills now"); the lead looks at the same frames before anything lands and is the final call. Sonnet lanes still never run headless-PNG or screenshot loops — briefs name the expected visual outcome and the lead renders and looks after the commit. Obsolete when: Sonnet-tier lanes show real visual judgment and Peter re-approves.

K3 serves at roughly 30–55 tokens/s, so on the Kimi profile the lead plans tersely: no narration before spawning, briefs and verdicts only.

**Debug escalation ladder (hook-enforced, `probe-loop-guard.py`).** For "this looks wrong and it's not obvious why": (1) lead semantic review of the seam first — the cheapest oracle and the strongest against this codebase's wiring bugs; (2) still stuck → the consult seat, read-and-discuss only; (3) instrument probe loops last, delegated to lanes with the evidence table in the brief, never lead-run. The hook counts lead probe actions and same-file edit→run→re-edit loops: warns at 3, denies at 6 until `/tmp/manifold_seam_review.md` (the evidence table) exists.

## The consult seat

Triggers:
1. **Design fork** the audit can't kill (`docs/DESIGN_AUTHORING.md` section 5 (Foreseeing the plausible-wrong turn)). One focused question.
2. **Debug ladder step 2** — a fresh strong read when the lead's seam review stalls.
3. **Pre-wave sanity check** — attack the brief set for wrong fix shapes, non-disjoint lanes, over-deletion.

Contract: read-and-discuss only — no Edit/Write, no spawns, no commits. Kimi profile: `cc-fleet subagent kimi --prompt-file <brief> --profile slim-ro --background`. Anthropic profile: a Fable fork or `model: "fable"` agent told it is read-only. Every consult brief carries a hard budget and a partial-report checkpoint at half budget; output past budget is discarded, not awaited. The lead integrates and owns the call.

## The brief contract

Slow flows come from agents re-deriving what the lead already knows. Every lane brief carries:

- **Established findings** with file:line anchors — never send an agent exploring for what the lead already knows.
- **Exact scope** — the files it may touch; read-only profile for investigations.
- **The gate commands** it must run and what "done" means. Lanes run their own gates and may iterate at most twice per gate, then stop and report verbatim.
- **Prescriptive imperatives** ("Run `cargo clippy -p manifold-ui -- -D warnings`", "Read `path:line`"), not "check X" — weaker models skip tool calls on soft phrasing.

## Verification

One strong verify pass per lane before landing: refute the diff against the brief and the gate, check citations, rerun the gate. Two weak passes don't sum to a strong one. Small lanes, frequent landing: 2–3 commits per phase beats one hours-long wave.

## Native provider lanes

Kimi profile only. Lanes are ordinary Agent-tool subagents; the seat's env routes every API call through the litellm proxy (127.0.0.1:4000) and remaps the four model slots, so `model: "haiku"` is a K2.7 lane, steerable via SendMessage and itemized in the proxy ledger. `k3m` pins the lead to K3 via `ANTHROPIC_MODEL=k3`; slot config can never change the lead. Invariant: no two seats share a strong slot (`seat-identity.py`). Lane self-reports about identity are theater; the proxy log is the oracle for which model served a request. Mechanics and upgrade hazards: the `litellm-seat-proxy` memory and `docs/PROVIDER_OPERATIONS.md`.

`cc-fleet spawn` (tmux teammates) is denied for every tier (`cc-fleet-tier-guard.py`): headless lanes are invisible in the UI and unreachable by SendMessage. `cc-fleet subagent` one-shots remain for consults; `cc-fleet run` remains the lead's launcher.

Kimi gotcha: `kimi-for-coding` on the endpoint is K2.7, not K3. Kimi bills cache reads, which are ~90% of lane volume, so K3 is only cheap against the flat plan window.

## Unattended multi-slice runs

Use when a phase's judgment is done and only mechanical bulk remains.

- **Decisions are files, not messages.** Rulings go to `.claude/orchestration/decisions.md` (append-only, lead writes); the queue and parked items live in files. Chat messages cross agent loops mid-flight; a seat re-reads the decisions file before pausing on any fork.
- **Design rulings outlive the night:** any mid-wave design decision is mirrored into the owning design doc as a numbered D-entry, same session, before the wave lands.
- **Skip-and-park, never block:** friction not covered by a standing decision parks the slice; the queue continues.
- **Every gate is an exit code** — that is what makes the loop scriptable and the night unattended.
- **One heavy build machine-wide:** full sweeps go through `.claude/scripts/with-build-lock.sh`.
- **Command hygiene for auto mode:** single-purpose commands, no `$()` writes or repo-path redirects (`/tmp` and `/dev/null` are fine). A blocked command = park, never retry variants.
- **Scope fence per night**, written into the queue: only pre-decided mechanical phases run.
- **A report is a trigger, never a stopping point:** act on a lane completion within standing authority immediately; ending a turn on a status summary is the observed stall.
- **Context ceiling for worker seats is hook-enforced** (`context-ceiling-guard.py`): warn 150K, stop 200K with a wrap-up lane (commit, handoff, report). The lead and any seat Peter is typing into are exempt. A well-shaped worker never nears 150K; hitting it is the defect signal. Rationale: `docs/TOKEN_ECONOMICS.md` section 3c (Cost inside a session grows without limit).
- **Nudges are idempotent:** the lead either nudges or dispatches for a slice, never both.
