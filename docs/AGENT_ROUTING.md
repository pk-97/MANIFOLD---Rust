# Agent Routing — task shape → model, seat, gate

**Status:** ACTIVE · 2026-10-01 rewrite to Claude models only (Peter, 2026-10-01). Authoritative staffing and routing policy; CLAUDE.md section Agents points here. The Kimi, open-weight and proxy material is retired: `docs/archive/AGENT_ROUTING_PROVIDER_LANES.md`.

## Slots and effort

Slot names reach the Claude models directly: `fable` is Fable 5.1, `opus` is Opus 5.5, `sonnet` is Sonnet 5.5, `haiku` is Haiku. The agent-launch guard prints the live map on every deny (`live map: …`); trust that over this page if they disagree. Every seat defaults to LOW effort; raising it is a per-task call for a named hard problem.

## The roster

| Seat | Model | Work |
|---|---|---|
| Lead | Opus 5.5, or Fable 5.1 for the hardest design sessions | Design, judgment, briefs, review, landing decisions. Owns every decision and every landed diff. The only orchestrator. |
| Opus lane | `opus` | Physics, GPU solvers and shaders, diagnosis, audits, anything that can hard-lock the Mac. |
| Fable lane | `fable` | Hard whole-system root-cause work. |
| Sonnet lane | `sonnet` | Fully decided work: app and UI wiring, tooling scripts, doc sweeps, test and flow runs, mechanical code moves. |
| Landing seat | `sonnet` | Runs the landing protocol on a lead-approved branch. See section The landing seat. |
| Consult | Fable fork | Read-and-discuss only. See section The consult seat. |

Haiku: Haiku 4.5 is not used for lanes (Peter, 2026-07-29: it stalls and needs nudges). Haiku 5.5 may be used where it makes sense once it releases (Peter, 2026-10-01).

Never to Sonnet lanes: graph semantics, GPU and kernel work, undo and lifecycle, design judgment, anything whose fix shape isn't decided. That work goes to the lead, or to an Opus or Fable lane under a written brief. Design calls stay with the lead either way.

There is no dispatcher seat. The clerical loop (pop queue, run gates, park failures) belongs to workflow scripts, exit-code gates and hooks.

## Standing practice

- **Owner seats.** Each area gets one long-lived owner seat. About three are live at once. The lead continues an owner with `SendMessage` instead of spawning a fresh agent, so the seat keeps what it learned. A fresh spawn happens only when the owner hits the context ceiling (section Unattended multi-slice runs) or fails twice; the outgoing owner writes a handoff to its branch or findings doc first.
- **Port from the reference.** When a reference implementation exists, the brief says port from it and name every deviation, with the reason, in the report. Silent divergence from a working reference is a brief violation.

## The landing seat

The happy path of a landing is mechanical: fetch, merge `origin/main` into the branch, `scripts/landing_gate.py --repo <worktree path>` (agents cannot cd, and HEAD on main's checkout is the base, so the gate refuses there), `git merge --no-ff` to main with a `Closes:` trailer, push, close the beads. Sonnet does that fine. What it can't be trusted with is judging whether a failure is simple, so it never judges:

- **Several ready branches land as one batch**, not one gate each (the gate is 30-60 minutes): `scripts/land_wave.py --batch <branch>[@<tip>] ...` merges them in queue order, gates once, drops a red culprit by name, and lands one merge listing every branch and tip. A single ready branch still lands alone. Protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).
- It lands only a branch the lead has reviewed and named in the brief.
- **Any non-zero exit, merge conflict, hook deny, or output it doesn't recognise → stop and report verbatim. No fix attempts, no retries with variations.** The lead takes it from there.
- It never force-pushes, never `branch -f`, never edits code.
- Report in the final text turn: the merge SHA, the gate summary line, the beads closed.

## The steering model

- **A judgment-tier model is the only orchestrator.** The lead. Never a lane model over lane models, at any depth.
- **The lead steers before the lane spawns.** Every brief names the existing system the work rides on (the reuse target) and the conviction test that must fail before the fix. Building a parallel path is a brief violation.
- **Lanes make exactly ONE commit, then STOP and report.** The lead reviews that commit before the lane continues. Wrong direction always shows in the first diff. An owner seat continued via `SendMessage` follows the same rule per continuation: one commit, stop, report.
- **A lane's report is its final text turn, never a SendMessage.** The harness delivers that turn to the lead; a SendMessage copy doubles it. SendMessage is for mid-flight questions and for the lead's continuations.
- **Lanes have no landing rights.** Lane branches are safe to abandon.
- **Lanes never spawn agents.** Only the lead spawns.
- **Decisions flow up.** "Existing system doesn't cover X" or "this needs a new module" = stop and report, never improvise.
- **Review is the throttle.** About three owner seats live at once, plus short Sonnet lanes while review keeps up. Landing never outpaces review.
- **Resume note in every brief.** Lane state = branch + findings doc, recoverable by the next session.
- **Work items are beads.** Briefs reference bead ids.
- **Lane health is scheduled, never Peter's job.** `scripts/fleet_health.py` is the automated check for stalled, blocked and unreviewed agent work; the lead reads its report, not the lanes. Two consecutive idle checks with no report = stalled: message once, then stop the lane and escalate.
- **A failing lane is respawned once, never taken over.** Stop it, carry what it learned into a fresh brief, respawn. A second failure means the task was shaped wrong: the lead re-shapes it (with the consult seat if stuck). The lead does not grind lane work itself.
- **Briefs restate the invariants every time:** one commit then stop, pathspec commits, no landing, worktree via the slot ring, explicit `model`, LOW effort.
- **Launch defects are one deny that spells the corrected call** (`agent-launch-guard.py`). Names are `<slot>-<descriptive-task>`.

## Lead token economy

Lead context is the scarcest resource in the rig. Reach for cheap seats before doing bulk searching and reading yourself: Sonnet lanes for tool-using recon and bounded mechanical asks.

What never delegates: verification of evidence the lead acts on. A weaker model's omissions are invisible in its own summary, so the lead spot-checks the underlying code or data. **Visual verification: judgment-tier seats only.** Opus 5.5 workers render and judge their own headless stills as part of their gate (Peter re-approved 2026-09-29: "Opus 5.5 has strong visual skills now"); the lead looks at the same frames before anything lands and is the final call. Sonnet lanes never run headless-PNG or screenshot loops: briefs name the expected visual outcome and the lead renders and looks after the commit. Obsolete when: Sonnet-tier lanes show real visual judgment and Peter re-approves.

**Debug escalation ladder.** For "this looks wrong and it's not obvious why": (1) lead semantic review of the seam first, the cheapest oracle and the strongest against this codebase's wiring bugs; (2) still stuck → the consult seat, a Fable fork reading the seam fresh, read-and-discuss only; (3) instrument probe loops last, delegated to an Opus diagnosis lane with the evidence table in the brief, never lead-run. Three probes without a written evidence table is the signal to step back to (1).

## The consult seat

Triggers:
1. **Design fork** the audit can't kill (`docs/DESIGN_AUTHORING.md` section 5 (Foreseeing the plausible-wrong turn)). One focused question.
2. **Debug ladder step 2.** A fresh strong read when the lead's seam review stalls.
3. **Pre-wave sanity check.** Attack the brief set for wrong fix shapes, non-disjoint lanes, over-deletion.

Contract: read-and-discuss only. No Edit/Write, no spawns, no commits. A Fable fork, or a `model: "fable"` agent told it is read-only. Every consult brief carries a hard budget and a partial-report checkpoint at half budget; output past budget is discarded, not awaited. The lead integrates and owns the call.

## The brief contract

Slow flows come from agents re-deriving what the lead already knows. Every lane brief carries:

- **Established findings** with file:line anchors. Never send an agent exploring for what the lead already knows.
- **Exact scope**: the files it may touch; read-only profile for investigations.
- **The gate commands** it must run and what "done" means. Lanes run their own gates and may iterate at most twice per gate, then stop and report verbatim.
- **Prescriptive imperatives** ("Run `cargo clippy -p manifold-ui -- -D warnings`", "Read `path:line`"), not "check X". Sonnet skips tool calls on soft phrasing.
- **The reference to port from**, when one exists, and the instruction to name every deviation.
- **The source and everything made from it.** Name the file that defines the contract and every file generated from or checked against it: builder, shipped JSON, catalog, UI flows, goldens. Regenerate from the source. A golden is refreshed only for a change the brief names, never because it fails.
- **The fixes a merge must keep.** Name the other branch's fixes and the tests that prove them. After merging main, run those tests, even when the merge was clean.

## Verification

One strong verify pass per lane before landing: refute the diff against the brief and the gate, check citations, rerun the gate. Verification never delegates to the seat that wrote the diff. Two weak passes don't sum to a strong one. Small lanes, frequent landing: 2–3 commits per phase beats one hours-long wave.

## Unattended multi-slice runs

Use when a phase's judgment is done and only mechanical bulk remains.

- **Decisions are files, not messages.** Rulings go to `.claude/orchestration/decisions.md` (append-only, lead writes); the queue and parked items live in files. Chat messages cross agent loops mid-flight; a seat re-reads the decisions file before pausing on any fork.
- **Design rulings outlive the night:** any mid-wave design decision is mirrored into the owning design doc as a numbered D-entry, same session, before the wave lands.
- **Skip-and-park, never block:** friction not covered by a standing decision parks the slice; the queue continues.
- **Every gate is an exit code.** That is what makes the loop scriptable and the night unattended.
- **One heavy build machine-wide:** full sweeps go through `.claude/scripts/with-build-lock.sh`.
- **Command hygiene for auto mode:** single-purpose commands, no `$()` writes or repo-path redirects (`/tmp` and `/dev/null` are fine). A blocked command = park, never retry variants.
- **Scope fence per night**, written into the queue: only pre-decided mechanical phases run.
- **A report is a trigger, never a stopping point:** act on a lane completion within standing authority immediately; ending a turn on a status summary is the observed stall.
- **Worker seats rotate at about 200K context:** commit, handoff, report, respawn. A well-shaped worker never nears 150K; hitting it is the defect signal. Rationale: `docs/TOKEN_ECONOMICS.md` section 3c (Cost inside a session grows without limit).
- **Nudges are idempotent:** the lead either nudges or dispatches for a slice, never both.
