# Widget-Tree / Queryable-UI-Layer — Design-Session Intake

**Status: INTAKE for a dedicated Fable design session · 2026-07-20 · written by the Wave-1/1.5 orchestrator session. Not a design doc — the design session it seeds produces one.**

## Mission

Design the queryable widget/param layer at the UI↔engine boundary — the structural fix that deletes the hand-written transcription code in the god files (`param_card.rs`, `inspector.rs`, `state_sync.rs`, `scene_setup_panel.rs`). This is a load-bearing, non-delegable piece. It is explicitly NOT laneable: a lane briefed with it recreates the scene panel at 10× scale. You design it yourself, whole.

## Ground rules for the design session

- Method: the deliverable conforms to `docs/DESIGN_DOC_STANDARD.md`.
- Design only — no implementation, no lanes, until Peter approves the doc. Decomposition of the god files FOLLOWS this design; splitting before killing the duplication spreads the mess.
- The testing doctrine is a hard constraint: the layer must make state-level testing (hit-test geometry as math, click→command dispatch, display-value resolution) the natural default. Pixels are for looking, not asserting.
- An Opus review agent is available at a genuine design fork, a Fable consult only if it is very complex (`docs/AGENT_ROUTING.md` section Reviews and the consult seat).

## Must-reads, in order

1. `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` — the autopsy of the failure class this layer kills.
2. Exemplar state-level tests in-tree: `ui_bridge/inspector.rs` (`undo_baseline`, `mapping_undo_baseline`, `bug_266_tab_pin` — the last landed 2026-07-20).
3. `docs/UI_ARCHITECTURE_OVERHAUL.md` + `UI_ARCHITECTURE_AUDIT.md (deleted)` — prior architecture read (audit is archived, still the best crate map).
4. Recent structural state: BUG-267 (inspector-duplicated-card-lists)'s unification landed 2026-07-20 (one card storage keyed by scope in `inspector.rs`); build on it, don't re-derive.

## Coordination

- Wave 2 (BUG-265 root fix, state-test families, drag survey) runs from the original orchestrator session in parallel. It touches `inspector.rs`/tests. Your session writes ONLY the design doc (+ `gen_docs_index.py`) until approval — zero code, zero conflicts.
- Worktree slots, landing protocol, all CLAUDE.md rules apply as usual when you later orchestrate the build.

## Resume-note

If this session dies: state = this doc + whatever draft exists in `docs/`. Any fresh Fable session re-enters from here.
