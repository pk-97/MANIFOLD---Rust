# Agent Routing — task shape → model, profile, gate

**Status:** ACTIVE 2026-07-19 · steering model added 2026-07-20 (Peter's directive after the overnight-wave autopsy; plan: `docs/SYSTEM_UPGRADE_2026_07_PLAN.md`). Authoritative staffing/routing policy. CLAUDE.md §Agents and the `agent-model-staffing-preferences` memory are pointers here.

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
