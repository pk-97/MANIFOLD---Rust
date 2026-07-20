# System Upgrade Plan — 2026-07-20

**Status: ACTIVE — Step 1 in progress (Fable, solo). Peter approved 2026-07-20.**
This is the reviewed plan artifact for the codebase + orchestration upgrade. Briefs are cut from THIS doc, never from chat. Adversarially reviewed by a Fable fork 2026-07-20; its 8 findings are folded in below.

## Diagnosis (short)

The 2026-07 overnight Sonnet waves failed the same way everywhere: agents built parallel infrastructure instead of riding existing systems (scene panel's imitation exposure layer, hand-copied param tables), and the gate verified the imitation (PNG/flow tests green while click paths were dead). Sonnet-orchestrating-Sonnet rubber-stamped green. The god files (param_card, inspector, state_sync, scene_setup_panel) are transcription code at the UI↔engine boundary — the same disease. Full autopsies: `SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md`, `INSPECTOR_DRAG_TAB_FINDINGS.md`, undo baseline `1bdb69a9`/`573b50ea`, `VERIFICATION_DEBT.md`.

## Operating model (supersedes prior practice; AGENT_ROUTING.md carries the rules)

- Fable (or K3 when Fable is out of window) is the ONLY orchestrator. Never Sonnet-over-Sonnet.
- Fable **steers**: chooses the approach, names the reuse target and conviction test in every brief, checks each lane's first commit.
- Lanes make **exactly one commit, then STOP and report**. Lanes have NO landing rights — only the top session merges to main.
- "Existing system doesn't cover X" is a report up, never a license to build. Any new helper module/harness = stop.
- Up to 8 lanes allowed; **review is the throttle** — diffs queue for Fable review, review never queues for diffs.
- Every wave's briefs get an adversarial Fable-fork pass before spawning.
- Each brief carries a resume-note: if the top session dies, lane state = branch + findings doc, recoverable.

## Step 1 — Fable solo, main session, BEFORE any lane (in progress)

1. This plan doc. ✔
2. CLAUDE.md rewrite: rules kept, incident stories stripped to pointers (~35KB → ~10KB).
3. MEMORY.md → true index (no status phrasing).
4. AGENT_ROUTING.md + FLEET_ORCHESTRATION.md: fold in the operating model above.
5. BUG_BACKLOG.md split: closed entries → `docs/BUG_ARCHIVE.md` (solo because every lane writes to the backlog — not laneable).
6. Hook trim: DEFERRED to its own careful pass — behavior changes under live protocol are riskier than the bloat.

## Wave 1 — lanes (COMPLETE 2026-07-20 — all four landed same day: W1-A `e8f066de`, W1-C `43c9d3d1`, W1-D `726de5a0`, W1-B `7915d4f3`. Full gpu-proofs suite green post-parity-removal. First wave under the steering model: 4/4 briefs executed to the letter, zero improvised mechanisms; lane reports caught one orchestrator-table error, adversarial pass caught 2 blockers pre-spawn.)

- **W1-A doc cull:** move superseded docs to `docs/archive/` from a list Fable writes; regen index.
- **W1-B gate cleanup:** CORRECTED 2026-07-20 at briefing — BUG-252's 8 flows were retargeted and fixed 2026-07-18 (`f101a585`/`d28bfff4`), NOT dead; only `scene-setup-scrub-fine.json` (BUG-240) dies. Scope now: that one script + retire ALL generated-vs-hand kernel parity tests and their hand-kernel oracles (Peter's ruling 2026-07-20 — migration scaffolding; fusion proofs and freeze proofs KEPT, headless render oracle untouchable) + look-oracle demotion note in HEADLESS_UI_HARNESS.md. Exact lists: the Wave 1 brief set (adversarially reviewed 2026-07-20, 11 findings folded).
- **W1-C BUG-266:** tab pin dies on incidental selection changes — decouple pin invalidation from `selection_version` (disjoint: `ui_state.rs`/`state_sync.rs`).
- **W1-D BUG-267:** unify master/layer card vecs in `inspector.rs` (the wave's real-code lane; structural, goes BEFORE BUG-265 which touches the same lines).

## Wave 1.5 — comment/noise sweep (COMPLETE 2026-07-20 — W15-C `f23eb8fa`, W15-D `dc95ea94`, W15-B `6ae3cd12`, W15-A `b8edad81`. Net ~−600 comment lines + 11 design-doc headers de-genealogized; board verified stable. Finding: majority of flagged comments were live constraints with BUG tags — codebase comments less rotten than assumed. One discipline breach: W15-A spawned sub-agents against the brief; contained by lane self-review + orchestrator sampling; one over-deletion caught and reverted by the lane, one live DEBT note restored at landing.)

The class: history narrated in code comments — dates, BUG-NNN provenance, "previously/used to", status-corrected notes. Measured 2026-07-20: ~1,400 such comment lines across 265 files in `crates/`. Git already holds all of it; in code it is context tax on every session and agent. The CLAUDE.md comment rule stops new instances; this wave clears the backlog.

- **Keep-rule (the whole judgment, decided up front):** a comment survives only if it states a CURRENT constraint the code can't show (`never-unify-cvdisplaylinks`-class warnings survive; "renamed 2026-06-11, was FooBar" dies). Fable writes the rule + exemplar diffs into the wave brief; lanes execute mechanically, delete-only diffs; ambiguous = keep + report.
- **Lanes:** cheap Sonnet at LOW effort, one per crate cluster, one commit each, standard review-then-land.
- **Docs half:** shipped design docs' status headers carrying correction genealogy → current state + git pointer, rest deleted. Same keep-rule, same lanes.
- Sequenced after Wave 1 (comment diffs in `inspector.rs`/`state_sync.rs` would collide with W1-C/D).
- **Input from W1-B's report — orphan-suspect audit rides this wave:** kept-per-table `.wgsl` files with zero remaining functional references in their own file (`bend_mesh`, `extrude_curve`, `revolve_curve`, `taper_mesh`, `tube_from_path`, `twist_mesh`, `morph_mesh`, `push_along_normals`); `fluid_scatter_3d.wgsl` (both sharers' gpu_tests now empty — re-verify the "shared" claim); `aces_tonemap_compute.wgsl` (verify `src/tonemap.rs` production usage). Delete-only-if-unreferenced, same rule as W1-B.

## Wave 2 — lanes

- **W2-A test families:** state-level UI tests copied from exemplars Fable hand-writes (hit-test geometry math; click→command dispatch; display-value resolution à la BUG-260 conviction test). Lanes replicate patterns; zero new infrastructure permitted.
- **W2-B BUG-265:** hit-test against live tree bounds (root fix), atop W1-D. SEQUENCING INVERTED at briefing (2026-07-20, fork-verified): W2-B authors the geometry conviction tests itself — they assert private fields reachable only from inspector.rs's in-file `mod tests` — and W2-A replicates the test families AFTER W2-B lands, using them + `bug_266_tab_pin` as exemplars.
- **W2-C drag scoping:** Peter reported drag-and-drop broken beyond cards — survey clip/timeline/other drag surfaces, findings doc only.

## Testing doctrine (agreed 2026-07-20; governs Wave 2 and all UI work)

Pixels are for looking, not asserting. Nearly every UI bug of 2026-07 was a state/wiring bug with a visual symptom — PNG assertions are slow, GPU-bound, and green while click paths are dead. The gate tests state on the REAL dispatch path (real EditingService, real state sync): hit-test geometry as pure math, click→command dispatch, display-value resolution (BUG-260 conviction test and the undo baseline `1bdb69a9` are the only permitted patterns — replicate, never invent harness). Headless PNG render stays as an on-demand look oracle for humans/Fable, out of the automated gate. The structural fix behind all of it: a queryable widget/param layer at the UI↔engine boundary — the god files (param_card, inspector, state_sync, scene_setup_panel) are hand-written transcription this layer deletes. That design is Fable-authored, NON-DELEGABLE; a lane briefed with it recreates the scene panel at 10× scale.

## Ongoing, non-delegable

- **Fable:** widget-tree / queryable-UI-layer design doc (the load-bearing structural fix; explicitly NOT laneable — a Sonnet lane here recreates the scene panel at 10× scale). Rides on UI_HARNESS_UNIFICATION groundwork.
- **Peter:** in-app acceptance pass on the undo fixes (`573b50ea` — never user-verified); scene-panel §5 decisions.

## Later (blocked/queued)

- K3 verification lanes per surface when K3 usage resets.
- Hook trim pass (Step 1.6).
- `manifold-core/effects.rs` dead-mass audit.
- God-file decomposition follows the widget-tree design — splitting before killing the duplication just spreads the mess.
