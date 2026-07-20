# Fleet Orchestration — Two-Layer Model

How Peter and the top-level Fable session run parallel work through cc-fleet panes. Simple by design.

## The two layers

**Top orchestrator (Fable, this session).** Holds the map: what work exists, which lane owns it, its status. Owns briefs, routing, and landing. Never reads a lane's full transcript. Asks Peter before opening any pane.

**Lane orchestrator (one per `team` pane).** Runs one workstream in its own worktree slot. Does the work (or spawns its own subagents), runs the gate, and reports a short status back up. Peter watches the pane for detail; Fable watches for decisions.

```
Peter ── approves ──▶ Fable (map + briefs + landing)
                        │ spawns team pane per lane
                        ▼
                Lane orch ─▶ Lane orch ─▶ Lane orch
                (slot-N)     (slot-M)     (slot-K)
                        │ short report up
                        ▼
                Fable updates map, lands, reports to Peter
```

## The loop

1. **Propose.** Fable identifies an independent, tightly-specced lane and asks Peter to approve opening it. No pane opens without approval.
2. **Brief.** On approval, Fable writes the lane brief: file:line anchors, exact scope, gate command, definition of done. Acquires a worktree slot.
3. **Spawn.** One `team` pane = one lane. Lane orchestrator opens on its slot, verifies base tip, works.
4. **Report up.** Lane pushes a short status only — `done` / `blocked: <reason>` / `gate: <result>`. Never a transcript.
5. **Decide.** Fable reacts: land it, re-brief, or fetch the full transcript *only if blocked*.
6. **Land.** Fable owns the landing protocol (fetch, merge origin/main, gate, `--no-ff`, push). Releases the slot.

## Rules that keep it cheap and safe

- **Permission before every pane.** Fable never spawns autonomously.
- **Conclusions flow up, not transcripts.** The top layer's context is the scarce resource — protect it. Full read only on a block.
- **One lane = one pane = one slot.** No per-phase panes. Slots come from `agent-worktree.py acquire`; `POOL FULL` is a hard stop surfaced to Peter.
- **Routing unchanged.** `docs/AGENT_ROUTING.md` still governs: Fable leads, K3 consult-only, Sonnet/K2 mechanical bulk, no Opus lanes. tmux only changes how many run at once and that Peter can watch them.
- **Lanes must be genuinely independent.** If two lanes touch the same code, they're one lane. Don't parallelize non-parallel work.
- **Every lane obeys every CLAUDE.md rule** — worktree slots, pathspec commits, landing protocol.

## When NOT to use a pane

Read-only audit, a fork of this context, or a batch of same-transform files — those stay in-session (Agent tool / `cc-fleet subagent`). A pane is for sustained, watchable, multi-turn build work only.
