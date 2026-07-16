# SCENE_SETUP_PANEL_DESIGN.md P1+P2 — landed 2026-07-17 @ 40d31dd0

**Branch:** wave/scene-setup-panel · **Level reached:** L3 (scripted UI-flow interaction, run and read by the orchestrating session in both the worktree and the main checkout) / target L3 (§10)
**Doc status line (quoted verbatim):** IN PROGRESS — P1 (column + discovery + Environment/Fog) + P2 (Objects section) SHIPPED 2026-07-17; P3–P5 not implemented. Sonnet-executable, orchestrated overnight. BUG-193 (no object/light remove command) and BUG-194 (vertex count not computable from def) opened as honest escalations, not blocking. · 2026-07-16 · Fable 5 (design session with Peter)

## Gate results (verbatim)

All gates below were re-run independently by the orchestrating session, not taken on the executing workers' self-report (per DESIGN_DOC_STANDARD §8.5). Two apparent compile-error IDE diagnostics surfaced mid-session against files that had already been committed clean; verified stale (a mid-edit snapshot) by rebuilding directly both times — see Deviations.

`cargo build --workspace` (main checkout, post-merge): clean, `Finished` in 27.06s.

`cargo clippy --workspace -- -D warnings` (main checkout, post-merge): clean (2 pre-existing ObjC deprecation warnings in manifold-media, unrelated).

`cargo nextest run --workspace` (main checkout, post-merge): `3497 tests run: 3497 passed, 12 skipped`.

`cargo deny check bans`: `bans ok`.

`cargo run -p manifold-renderer --bin check-presets`: `53 presets: 53 ok, 0 failed` (includes the new `SceneStarter.json`).

Negative gates (§4): `rg "MutateProject|Arc<Mutex|Arc<RwLock" crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0 hits; `rg "Project\b" crates/manifold-renderer/src/node_graph/scene_vm.rs` → 0 hits; `layer.rs` and `ports.rs` untouched across the wave.

Round-trip: `scene_setup_fog_edit_survives_save_reload_and_scene_vm_re_shows_it` (manifold-renderer) — fog density 0.37 survives V1 JSON save/reload and `SceneVm::from_def` re-shows it. Pass.

L3 flows, re-run by the orchestrator in the MAIN checkout (real fixtures, not the worktree's copies):
- `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-fog-drag.json` — 13/13 steps ok. Opens the dock via the header "Scene" button, asserts Environment/`+ Add Fog` exist, adds fog, drags Density from 0.00 to 1.00 across 8 steps, asserts the value changed, undo key-combo dispatched.
- `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-object.json` — 8/8 steps ok. Opens the dock, asserts Objects/`+ Object` exist and "Object 3" does NOT yet exist, clicks `+ Object`, asserts "Object 3" now exists.

PNGs read by the orchestrator (affordance check — do controls read as clickable, not bare text): every D7 empty state (no-selection, no-scene, no-generator), the full panel on the real azalea glTF import (Environment/Fog live, Camera/Sun/Environment rows visible in the card), and the Objects section before/after `+ Object` on the same fixture. All buttons render as raised chips distinguishable from static labels; drag-value cells read distinctly from labels.

## Deviations from brief

- **P1 added real on-screen "Audio"/"Scene" header toggle buttons** (`crates/manifold-ui/src/panels/header.rs` + a parallel `MenuAction::Scene`). The brief assumed an existing on-screen "Audio" button beside which "Scene" would sit (D2); none existed — Audio Setup was reachable only via the View menu / ⌘⇧A. Judged a "moved anchor, re-derive" case rather than an escalation: D2 itself commits to a header toggle, so building both buttons (matching the existing `View::button` pattern) is the mechanical completion of the doc's own intent, not a new architectural choice. Verified as correct by the orchestrator.
- **P2 found and fixed a real P1 bug**: the `transform_k`/material trace looked for a bare `node.transform_3d` wired directly to `render_scene`, but the shipped shape (`AddSceneObjectCommand`, the glTF importer) wires it *inside* the object's group — every real object would have silently shown "no transform" forever. Fixed by tracing inside the group's scope; the Custom-row fallback (no group) still traces at root.
- **P2 found and fixed a P1 test-fixture ambiguity**: the P1 fog-drag flow's `under_text: "Fog"` selector matched ambiguously once the Objects section was added above it (this codebase's `param_card`-style flat-sibling layout has no per-row container — see also BUG-192, a pre-existing unrelated instance of the same `under_text` class found the same week on a different card). Fixed by giving rows stable `set_name` automation identities and repointing the flow. Also downgraded a vacuous "undo restores value" assertion to a plain smoke step — the headless harness runs no content thread, so `Cmd+Z` writes into a dead channel; it never proved reversion and the flow no longer claims it does.
- **Stale IDE diagnostics during both P1 and P2**, showing plausible-looking compile errors (missing module, non-exhaustive match, mismatched delimiter) against files already committed clean. Both times verified via a direct `cargo build`/`cargo clippy` in the worktree at the committed HEAD — genuinely clean; the diagnostics were mid-edit snapshots the editor hadn't refreshed. Not a landing concern, noted here so a future orchestrator doesn't lose time re-deriving the same check.

## Shortcuts confessed (rolled up from phase reports)

- P1: Fog's Color/Ambient Tint rows and Environment's Mode toggle/HDRI Browse render read-only in v1 (only Intensity/Fill/Density/Height-Falloff are live sliders) — D4 commits all of these; scoped down for the phase's time budget, not hidden (all real, all named). `Scene Starter.json` filed as `SceneStarter.json` (existing bundled-preset filename convention, no space) and uses `node.grid_mesh` directly rather than the doc's literal `generate_grid_mesh + triangulate_grid` phrasing — confirmed equivalent via `graph_tool validate`/`fusion` + `check-presets`. No literal `PartialEq`-based `scene_panel_slider_emits_card_identical_command` test (`SetGraphNodeParamCommand` has no `PartialEq`); proved via matching constructor + the round-trip + L3 flow instead. The byte-identity gate (panel open vs closed) is proved by a dedicated layout unit test plus code-path inspection (zero content-thread code touched), not a separate show-render harness run.
- P2: Remove control not built — see BUG-193 (escalated, not shipped as a stub). Header vertex count not built — see BUG-194 (escalated, not fabricated). D7's "Import Model…" empty-state button correctly deferred to P4 per the brief's own literal P1 deliverable list.

## Verification debt

None opened. The one deferred item that could look like debt — Remove object/light control, header vertex count — are tracked as BUG-193/BUG-194 (genuine escalations per the doc's own §8 contract: "any panel control with no existing command to dispatch" pauses rather than improvises), not silent gaps.

## Click-script for Peter (≤2 minutes)

1. Open the app on a project with an imported glTF generator layer selected (or any generator layer). Click the new **Scene** button in the header (beside **Audio**) — expect: a right-hand dock column slides open, same animation as Audio Setup.
2. With no layer selected — expect: "Select a layer to set up its scene."
3. Select a 2D generator layer (e.g. Plasma, Noise Field) — expect: "This generator has no 3D scene." + an **Open Graph Editor** button.
4. Select an empty/no-generator layer — expect: **New 3D Scene** button (assigns the bundled starter preset: grid floor, lit cube, sun + fill light, fog, orbit camera).
5. On a real imported scene: drag the **Fog Density** slider (or click **+ Add Fog** first if fog isn't present) — expect: the preview haze changes live, no graph editor involved.
6. Click **+ Object** in the Objects section — expect: a new "Object N" row appears with default transform/material rows; rename any object's name field — expect: the card section for that object renames too (existing rename-sweep behavior).
