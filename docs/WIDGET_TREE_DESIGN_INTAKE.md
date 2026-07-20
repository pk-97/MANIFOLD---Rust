# Widget-Tree / Queryable-UI-Layer — Design-Session Intake

**Status: INTAKE for a dedicated Fable design session · 2026-07-20 · written by the Wave-1/1.5 orchestrator session. Not a design doc — the design session it seeds produces one.**

## Mission

Design the queryable widget/param layer at the UI↔engine boundary — the structural fix that deletes the hand-written transcription code in the god files (`param_card.rs`, `inspector.rs`, `state_sync.rs`, `scene_setup_panel.rs`). This is the load-bearing piece of `docs/SYSTEM_UPGRADE_2026_07_PLAN.md` (§Ongoing, non-delegable). It is explicitly NOT laneable: a lane briefed with it recreates the scene panel at 10× scale. You design it yourself, whole.

## Ground rules for the design session

- Method: `docs/DESIGN_AUTHORING.md` first, whole; deliverable conforms to `docs/DESIGN_DOC_STANDARD.md`.
- Design only — no implementation, no lanes, until Peter approves the doc. Decomposition of the god files FOLLOWS this design; splitting before killing the duplication spreads the mess.
- The testing doctrine (plan doc §Testing doctrine) is a hard constraint: the layer must make state-level testing (hit-test geometry as math, click→command dispatch, display-value resolution) the natural default. Pixels are for looking, not asserting.
- K3 consult is available at a genuine design fork (AGENT_ROUTING.md §consult triggers).

## Must-reads, in order

1. `docs/SYSTEM_UPGRADE_2026_07_PLAN.md` — diagnosis + doctrine (short).
2. `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` — the autopsy of the failure class this layer kills.
3. `docs/INSPECTOR_DRAG_TAB_FINDINGS.md` — BUG-265/266/267; the three-geometry-sources disease (BUG-265 root fix may land via Wave 2 before you finish — check `git log` / backlog).
4. `docs/UI_HARNESS_UNIFICATION_DESIGN.md` — the groundwork this rides on (read the Reframe block first, per its own header).
5. Exemplar state-level tests in-tree: `ui_bridge/inspector.rs` (`undo_baseline`, `mapping_undo_baseline`, `bug_266_tab_pin` — the last landed 2026-07-20, `fcd4c084`).
6. `docs/UI_ARCHITECTURE_OVERHAUL.md` + `docs/archive/UI_ARCHITECTURE_AUDIT.md` — prior architecture read (audit is archived, still the best crate map).
7. Recent structural state: BUG-267's unification landed 2026-07-20 (`717f8910` — one card storage keyed by scope in `inspector.rs`); build on it, don't re-derive.

## Coordination

- Wave 2 (BUG-265 root fix, state-test families, drag survey) runs from the original orchestrator session in parallel. It touches `inspector.rs`/tests. Your session writes ONLY the design doc (+ `gen_docs_index.py`) until approval — zero code, zero conflicts.
- Worktree slots, landing protocol, all CLAUDE.md rules apply as usual when you later orchestrate the build.

## Resume-note

If this session dies: state = this doc + whatever draft exists in `docs/`. Any fresh Fable session re-enters from here.
