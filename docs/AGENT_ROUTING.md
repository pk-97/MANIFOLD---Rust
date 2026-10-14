# Agent Routing — task shape → model, seat, gate

**Status:** ACTIVE · Claude models only; no other harness or provider is in use. Authoritative staffing and routing policy; CLAUDE.md section Agents points here.

## Slots and effort

Slot names reach the Claude models directly: `fable` is Fable 5.1, `opus` is Opus 5.5, `sonnet` is Sonnet 5.5, `haiku` is Haiku. The agent-launch guard prints the live map on every deny (`live map: …`); trust that over this page if they disagree.

**Effort:** agents run at medium effort by default and at high for hard judgment work (diagnosis, GPU, design, reviews, audits). Never pin an agent to low.

## The roster

| Seat | Model | Work |
|---|---|---|
| Lead | Opus 5.5, or Fable 5.1 for the hardest design sessions | Design, judgment, briefs, review, landing. Owns every decision and every landed diff. The only orchestrator and the only seat that lands. |
| Opus lane | `opus` | Physics, GPU solvers and shaders, diagnosis, audits, anything that can hard-lock the Mac. |
| Fable lane | `fable` | Hard whole-system root-cause work. |
| Sonnet lane | `sonnet` | Fully decided work: app and UI wiring, tooling scripts, doc sweeps, test and flow runs, mechanical code moves. |
| Reviewer | The lead, with `opus` agents | Default for every review. See section Reviews and the consult seat. |
| Consult | Fable fork | Very complex reviews only. Read-and-discuss. |

Haiku: the newest Haiku runs retrieval and given commands, never judgment or edits.

Never to Sonnet lanes: graph semantics, GPU and kernel work, undo and lifecycle, design judgment, anything whose fix shape isn't decided. That work goes to the lead, or to an Opus or Fable lane under a written brief. Design calls stay with the lead either way.

There is no dispatcher seat. The clerical loop (pop queue, run gates, park failures) belongs to workflow scripts, exit-code gates and hooks.

## Standing practice

- **Shut a lane down when it reports.** Once its report is read, the lead stops the agent unless a follow-up brief for it is already planned. An idle agent is a token leak, and the harness can wake it to re-send its report. What it learned lives in its commit and report, so the next brief carries it to a fresh agent.
- **Port from the reference.** When a reference implementation exists, the brief says port from it and name every deviation, with the reason, in the report. Silent divergence from a working reference is a brief violation.

## Landing

Lanes commit once and stop; the lead commits, pushes and lands (CLAUDE.md section Hard rules). The lead lands a reviewed branch with `scripts/land_branch.py <branch> --worktree <path> --message '...' [--close-bead <id> ...]`: it merges `origin/main` in, gates, merges `--no-ff` to main, pushes and closes the beads in one command. Follow it with `scripts/watch_land.py --kind land`. Protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

- Land each reviewed branch on its own. `scripts/land_wave.py --batch` (one gate over several branches) is a fallback for a pile-up of ready branches, not the standard path.
- Never force-push and never `branch -f` main.

The lead acts on a red within two minutes of `scripts/watch_land.py` firing.
For a blocked lane, the lead unblocks it, runs the refused command, or parks it
within five minutes.

## The steering model

- **A judgment-tier model is the only orchestrator.** The lead. Never a lane model over lane models, at any depth.
- **The lead steers before the lane spawns.** Every brief names the existing system the work rides on (the reuse target) and the conviction test that must fail before the fix. Building a parallel path is a brief violation.
- **Lanes make exactly ONE commit, then STOP and report.** The lead reviews that commit before the lane continues. Wrong direction always shows in the first diff. A lane kept for a planned follow-up follows the same rule per continuation: one commit, stop, report.
- **A lane's report is its final text turn, never a SendMessage.** The harness delivers that turn to the lead; a SendMessage copy doubles it. SendMessage is for mid-flight questions and for the lead's continuations.
- **Lanes have no landing rights.** Lane branches are safe to abandon.
- **Lanes never spawn agents.** Only the lead spawns.
- **Decisions flow up.** "Existing system doesn't cover X" or "this needs a new module" = stop and report, never improvise.
- **Review is the throttle.** About three judgment lanes live at once, plus short Sonnet lanes while review keeps up. Landing never outpaces review.
- **Resume note in every brief.** Lane state = branch + findings doc, recoverable by the next session.
- **Work items are beads.** Briefs reference bead ids.
- **Lane health is the lead's job, never Peter's.** `scripts/fleet_health.py` checks orphaned slot edits, disk, the GPU lock and the nightly; it does not see Claude lanes (BUG-7eep8 (Claude-lane stall detection)). The lead watches Claude lanes through their completion notifications. Two consecutive idle checks with no report = stalled: message once, then stop the lane and escalate.
- **A failing lane is respawned once, never taken over.** Stop it, carry what it learned into a fresh brief, respawn. A second failure means the task was shaped wrong: the lead re-shapes it (with an Opus review agent if stuck). The lead does not grind lane work itself.
- **Briefs restate the invariants every time:** one commit then stop, pathspec commits, no landing, worktree via the slot ring, explicit `model`, effort (medium, high for hard judgment work), and the CLAUDE.md section How every session works rules (no inline python, no `cd`, never wait in the foreground, pre-commit checks plus `--cheap` gate, never gate a slot still being written).
- **Launch defects are one deny that spells the corrected call** (`agent-launch-guard.py`). Names are `<slot>-<descriptive-task>`.

## Lead token economy

Lead context is the scarcest resource in the rig. The lead does no hands-on work and reads only to review; agents do every search, read, edit and run.

Briefs and reports stay short. An agent re-reads its whole context on every call, and most calls write prose, so every padded line in a brief or a report is paid again on each later step. A brief carries findings and scope, not background; a report carries outcome, evidence and blockers, not narration.

What never delegates: verification of evidence the lead acts on. A weaker model's omissions are invisible in its own summary, so the lead spot-checks the underlying code or data. **Visual checks are verification, never a landing gate; the gates are the gates.** Opus seats may render and judge headless stills to verify their own work, and the lead may look at them in review. Sonnet lanes never run headless-PNG or screenshot loops: briefs name the expected visual outcome and an Opus lane renders after the commit. Peter's eye is the stage check after landing. Obsolete when: Sonnet-tier lanes show real visual judgment.

**Debug escalation ladder.** For "this looks wrong and it's not obvious why": (1) lead semantic review of the seam first, the cheapest oracle and the strongest against this codebase's wiring bugs; (2) still stuck → an Opus agent reading the seam fresh, read-and-discuss only (a Fable consult only when the seam is very complex); (3) instrument probe loops last, delegated to an Opus diagnosis lane with the evidence table in the brief, never lead-run. Three probes without a written evidence table is the signal to step back to (1).

## Reviews and the consult seat

The default reviewer is the lead, with `opus` agents for the reading and the adversarial pass. A Fable consult is only for a very complex review. Uses:
1. **Design fork** the audit can't kill (`docs/DESIGN_AUTHORING.md` section 5 (Foreseeing the plausible-wrong turn)). One focused question.
2. **Debug ladder step 2.** A fresh strong read when the lead's seam review stalls.
3. **Pre-wave sanity check.** Attack the brief set for wrong fix shapes, non-disjoint lanes, over-deletion.

Contract: read-and-discuss only. No Edit/Write, no spawns, no commits. An `opus` agent, or for a very complex review a Fable fork or `model: "fable"` agent, told it is read-only. Every review brief carries a hard budget and a partial-report checkpoint at half budget; output past budget is discarded, not awaited. The lead integrates and owns the call.

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
- **No context cap on worker seats:** long, hard tasks stay in one seat; a seat is replaced only when it fails twice.
- **Nudges are idempotent:** the lead either nudges or dispatches for a slice, never both.
