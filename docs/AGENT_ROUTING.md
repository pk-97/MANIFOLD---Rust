# Agent Routing — task shape → model, seat, gate

**Status:** ACTIVE · Claude models only; no other harness or provider is in use. Authoritative staffing and routing policy; CLAUDE.md section Agents points here.

## Slots and effort

Slot names (`fable`, `opus`, `sonnet`, `haiku`) reach Claude models directly. Which model each resolves to is the agent-launch guard's live map (`.claude/hooks/agent-launch-guard.py`, read from the session env and printed on every deny as `live map: …`); this page names no versions.

**Effort:** agents run at medium effort by default and at high for hard judgment work (diagnosis, GPU, design, reviews, audits). Never pin an agent to low.

## The roster

| Seat | Model | Work |
|---|---|---|
| Lead | Opus, or Fable for the hardest design sessions | Design, judgment, briefs, review, landing. Owns every decision and every landed diff. The only orchestrator and the only seat that lands. |
| Opus lane | `opus` | Physics, GPU solvers and shaders, diagnosis, audits, anything that can hard-lock the Mac. |
| Fable lane | `fable` | Hard whole-system root-cause work. |
| Sonnet lane | `sonnet` | Fully decided work: app and UI wiring, tooling scripts, doc sweeps, test and flow runs, mechanical code moves. |
| Reviewer | The lead, with `opus` agents | Default for every review. See section Reviews and the consult seat. |
| Consult | Fable fork | Very complex reviews only. Read-and-discuss. |

Haiku: the newest Haiku runs retrieval and given commands, never judgment or edits.

Never to Sonnet lanes: graph semantics, GPU and kernel work, undo and lifecycle, design judgment, anything whose fix shape isn't decided. That work goes to the lead, or to an Opus or Fable lane under a written brief. Design calls stay with the lead either way.

There is no dispatcher seat. The clerical loop (pop queue, run gates, park failures) belongs to workflow scripts, exit-code gates and hooks.

## Standing practice

- **Shut a lane down when it reports.** Once its report is read, the lead stops the agent. Follow-up work goes to a fresh lane whose brief carries the findings with file:line anchors; resume a reported lane only for a small fix to its own commit. A resumed lane re-reads its whole 200k+ context on every step, about three times what a fresh lane costs. An idle agent is a token leak, and the harness can wake it to re-send its report.
- **One lead session per campaign.** Each lead step re-reads the lead's whole context; past 200k that dominates the lead's cost. When a campaign lands, the next one starts in a new session from its bead and handoff notes.
- **Port from the reference.** When a reference implementation exists, the brief says port from it and name every deviation, with the reason, in the report. Silent divergence from a working reference is a brief violation.

## Landing

Lanes commit once and stop; the lead commits, pushes and lands (CLAUDE.md section Hard rules). The lead lands a reviewed branch with `scripts/land_branch.py <branch> --worktree <path> --message '...' [--close-bead <id> ...]`: it merges `origin/main` in, gates, merges `--no-ff` to main, pushes and closes the beads in one command. Follow it with `scripts/watch_land.py --kind land`. Protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

- Landing is per branch by default. Batch only while review keeps up: merge the reviewed branches into one merge branch and land that once with `land_branch.py`.
- Never force-push and never `branch -f` main.

The lead acts on a red within two minutes of `scripts/watch_land.py` firing.
For a blocked lane, the lead unblocks it, runs the refused command, or parks it
within five minutes.

## The steering model

- **A judgment-tier model is the only orchestrator.** The lead. Never a lane model over lane models, at any depth.
- **The lead steers before the lane spawns.** Every brief names the existing system the work rides on (the reuse target) and the conviction test that must fail before the fix. Building a parallel path is a brief violation.
- **Lanes make exactly ONE commit, then STOP and report.** The lead skims that commit before the lane continues; wrong direction always shows in the first diff. The adversarial agent review runs once per landing, not per commit (section Reviews and the consult seat). A lane kept for a planned follow-up follows the same rule per continuation: one commit, stop, report.
- **A lane's report is its final text turn, never a SendMessage.** The harness delivers that turn to the lead; a SendMessage copy doubles it. SendMessage is for mid-flight questions and for the lead's continuations.
- **Lanes have no landing rights.** Lane branches are safe to abandon.
- **Lanes never spawn agents.** Only the lead spawns.
- **Decisions flow up.** "Existing system doesn't cover X" or "this needs a new module" = stop and report, never improvise.
- **Review is the throttle.** About three judgment lanes live at once, plus short Sonnet lanes while review keeps up. Landing never outpaces review.
- **Resume note in every brief.** Lane state = branch + findings doc, recoverable by the next session.
- **Work items are beads.** Briefs reference bead ids.
- **Lane health is the lead's job, never Peter's.** `scripts/fleet_health.py` checks orphaned slot edits, disk, the GPU lock and the nightly; it does not see Claude lanes (BUG-7eep8 (Claude-lane stall detection)). The lead watches Claude lanes through their completion notifications. Two consecutive idle checks with no report = stalled: message once, then stop the lane and escalate.
- **A failing lane is respawned once, never taken over.** Stop it, carry what it learned into a fresh brief, respawn. A second failure means the task was shaped wrong: the lead re-shapes it (with an Opus review agent if stuck). The lead does not grind lane work itself.
- **Briefs restate the invariants every time:** one commit then stop, pathspec commits, no landing, worktree via the slot ring, explicit `model`, effort (medium, high for hard judgment work), and the CLAUDE.md section How every session works rules (no inline python, no `cd`, wait on your own builds and tests in the foreground and never end your turn while they run, pre-commit checks including the `--cheap` gate before the commit, never gate a slot still being written).
- **Launch defects are one deny that spells the corrected call** (`agent-launch-guard.py`). Names are `<slot>-<descriptive-task>`.

## Lead token economy

Lead context is the scarcest resource in the rig. What the lead does itself and what it delegates is in CLAUDE.md section How every session works.

Briefs and reports stay short. An agent re-reads its whole context on every call, and most calls write prose, so every padded line in a brief or a report is paid again on each later step. A brief carries findings and scope, not background; a report carries outcome, evidence and blockers, not narration.

What never delegates: verification of evidence the lead acts on. A weaker model's omissions are invisible in its own summary, so the lead spot-checks the underlying code or data. **Visual checks are verification, never a landing gate; the gates are the gates.** Opus seats may render and judge headless stills to verify their own work, and the lead may look at them in review. Sonnet lanes never run headless-PNG or screenshot loops: briefs name the expected visual outcome and an Opus lane renders after the commit. Peter's eye is the stage check after landing. Obsolete when: Sonnet-tier lanes show real visual judgment.

**Debug escalation ladder.** For "this looks wrong and it's not obvious why": (1) lead semantic review of the seam first, the cheapest oracle and the strongest against this codebase's wiring bugs; (2) still stuck → an Opus agent reading the seam fresh, read-and-discuss only; (3) instrument probe loops last, delegated to an Opus diagnosis lane with the evidence table in the brief, never lead-run. Three probes without a written evidence table is the signal to step back to (1).

## Reviews and the consult seat

The reviewer is the lead, with `opus` agents for the reading, the adversarial pass, design forks, debug ladder step 2 and pre-wave sanity checks. Fable is expensive: it gets one review per campaign, the very final one, confirming the branch is complete and free of slop and bugs before landing. Any other Fable use needs Peter's ask.

Contract: read-and-discuss only. No Edit/Write, no spawns, no commits. An `opus` agent, or for the final campaign review a `model: "fable"` agent, told it is read-only. The lead integrates and owns the call.

**One adversarial review per landing.** A lane builds a phase as small commits the lead skims one by one; one `opus` adversarial review covers the whole batch before it lands. Instrumentation-only changes and changes that never touch playback get no separate agent review.

**Budgets bound wandering, never coverage.** Every review brief carries a hard budget and a partial-report checkpoint at half budget, because an open-ended reviewer can read indefinitely and ignore stop messages. Every report, partial or final, lists what it did not read. Anything it did not read is unreviewed, not passed: it gets its own follow-up review, scoped to just that part, before the branch lands.

## The brief contract

Slow flows come from agents re-deriving what the lead already knows. Every lane brief carries:

- **Established findings** with file:line anchors. Never send an agent exploring for what the lead already knows.
- **Exact scope**: the files it may touch; read-only profile for investigations.
- **The gate commands** it must run and what "done" means. Lanes run each check once to the end, fix every failure it shows, then rerun only those failures by name; still red after that → stop and report verbatim. No repeated full runs; the one `--cheap` gate run is reused at landing. UI flows: a lane runs only the flows its change adds or edits, by name, never a family (`scene`) or the full suite; the landing gate runs the mapped set once. A flow red on main sits in the manifest's `expected_fail` with its bead, so it never forces a named-red rerun.
- **Prescriptive imperatives** ("Run `cargo nextest run -p manifold-ui <filter>`", "Read `path:line`"), not "check X". Sonnet skips tool calls on soft phrasing.
- **The reference to port from**, when one exists, and the instruction to name every deviation.
- **The source and everything made from it.** Name the file that defines the contract and every file generated from or checked against it: builder, shipped JSON, catalog, UI flows, goldens. Regenerate from the source. A golden is refreshed only for a change the brief names, never because it fails.
- **The fixes a merge must keep.** Name the other branch's fixes and the tests that prove them. After merging main, run those tests, even when the merge was clean.

## Verification

One strong verify pass per lane before landing: refute the diff against the brief and the gate, check citations, rerun the gate. Verification never delegates to the seat that wrote the diff. Two weak passes don't sum to a strong one. Small lanes, frequent landing: 2–3 commits per phase beats one hours-long wave.

## Lead discipline

- A bisect needs a green control: run the base alongside rung one. A uniform-red bisect means the baseline assumption is wrong.
- Lane gate reports are claims. Re-run the gates on the committed tree before accepting; the lane's build warmed the target dir, so it costs seconds.
- Check the commit file list against the brief's deliverables, then verify the deliverable is live on main (the test runs, the feature is reachable). A lane's reported sha is verified with `git cat-file -t <sha>` and the branch tip before anything is built on it.
- Files over 3000 lines: the lead writes a line-anchored implementation map into the brief; exploration briefs and giant files do not mix.
- A gate or verifier tooling change is re-run against the real commit it fixes, not only its synthetic self-test.
- Enumeration briefs are type-shaped: every constructor and consumer of the changed type, found by the type's own registry, never every file matching a string. The compiler or a typed test is the complete oracle.
- Outputs judged by reading (briefs, reviews, records) can be one-shot calls; outputs judged by running go to a lane.
- A lane never runs `git checkout` or `restore` on a file with the lead's uncommitted work. Commit lead work before delegating into the same file; scope reverts to the lane's own edits.
- A mispinned lane (the denial names a slot that is not its own) re-pins with `EnterWorktree(path=<slot>)` and verifies the tip, and never edits the tree it landed in.
- The lead records Peter's rulings itself, first-hand, the turn he states them, as plain rules; agents never write a ruling from a relayed brief.
- Check a bead title, report or summary at the source (`git show`, the code, the test output) before relaying it.
- Stamps come from a `date` run in its own call.
- Performance and physics changes are verified the way Peter runs them: in the app, or frame time on his project at its saved settings, against the main he last ran.
- A result that destroys the instrument (water explodes, frame goes black) stops the line: no landings on top until it is fixed or Peter calls it. An absolute ruling that cannot hold under overload gets one push-back with the physics before anything is built on it.
- Never land over a named red until a complete gate run's logs are read, every leg.
- Every landing or test failure gets a typed bead the same session, with symptom, cost and root cause or suspects.
- Briefs for temporary or diagnostic tooling carry a line budget and name what to leave out.

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
