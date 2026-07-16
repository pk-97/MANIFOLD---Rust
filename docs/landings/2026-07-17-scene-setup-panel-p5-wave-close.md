# SCENE_SETUP_PANEL_DESIGN.md P5 — landed 2026-07-17 @ 624be19a (wave close)

**Branch:** wave/scene-setup-panel · **Level reached:** L3 (scripted UI-flow interaction, real held-out asset merge in P4, direct testing of a shared-infra bug found while re-verifying this landing) / target L3 (§10)
**Doc status line (quoted verbatim):** SHIPPED (all phases P1–P5) 2026-07-17. P1 (column + discovery + Environment/Fog) + P2 (Objects section) + P3 (Lights + Camera sections) + P4 (Import Model merge, held-out warehouse+skull gate passed) + P5 (modifier stack: 3 splice commands + panel UI) all landed. Sonnet-executable, orchestrated overnight. BUG-193 (no object/light remove command), BUG-194 (vertex count not computable from def), BUG-195 (merge scale-sanity has no stored object radius, defaulted proxy), BUG-198 (headless `Key Z` doesn't reach Undo — found during P5, pre-existing harness gap) opened as honest escalations, not blocking.

## Gate results (verbatim)

`cargo build --workspace` (main checkout, post-merge): clean.

`cargo clippy --workspace -- -D warnings` (main checkout, post-merge): clean (2 pre-existing unrelated manifold-media ObjC deprecation warnings).

`cargo nextest run --workspace` (main checkout, post-merge): `3544 tests run: 3544 passed, 12 skipped`.

`cargo deny check bans`: `bans ok`.

Command inverse-pair tests (9, `manifold-editing/src/commands/graph.rs`): insert at end/position-0/nested-group/refused-on-unparseable, remove middle/first/last, move-to-front/move-to-end — verified to genuinely exist and pass, all asserting byte-equal `def` after undo.

Negative gates (§4): `rg "MutateProject|Arc<Mutex|Arc<RwLock" crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0 hits; `rg "modifier" crates/manifold-core/` → only pre-existing beat-modifier vocabulary, 0 scene-modifier storage hits (the wire chain is the only home for the stack, confirmed); `layer.rs`/`ports.rs` untouched since P4's landing tip; no shader/runtime files touched across P5's diff.

`graph_tool validate`/`fusion` on a post-splice def (`SceneStarter.json` + a Twist modifier): `OK`, `node.twist_mesh` correctly classified `pointwise` (fusable).

L3 flow, re-run by the orchestrator in the MAIN checkout: `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` — 22/22 steps ok. Adds Twist then Bend to an object, drags Bend's angle param, reorders the two modifiers (move-to-front), sends 4× Cmd+Z (a documented no-op per BUG-198 — undo is proven at the command level instead, per the report below).

PNG read by the orchestrator (affordance check): both objects show their live modifier stacks (Twist + Bend with up/down/remove chips, editable Axis/Angle/Center rows) and the curated 7-item "Add modifier:" chip grid (Bend/Twist/Taper/Inflate/Displace by Texture/Morph/Rotate) — all buttons read as distinct clickable chips, not bare text.

## A real bug found while re-verifying this landing (not caused by P5, exposed by it)

Re-running P1–P4's own previously-green flows against the fully-merged P1–P5 panel surfaced a genuine, pre-existing product gap, confirmed by direct testing rather than just code reading:

- **`scene-setup-light-cast-shadows-toggle.json` (P3's flow) broke again** — P5 added an "Add modifier:" section to every object row (rendered even with zero modifiers), which pushed everything below Objects down further, the same class of regression P4 caused to P3's flow. Diagnosed the same way: forced-failure `--dump` of the live widget tree found the Cast Shadows "−" button had moved from y=768 (P4-era) to y=972. Fixed by repointing the click; re-verified 6/6 in both the worktree and the main checkout.
- **`scene-setup-add-fog-drag.json` (P1's flow) is now genuinely unreachable, and this one is NOT a coordinate-fix** — the Fog section's y-position (~1232–1244) now exceeds the window's own rendered height (1216px), not just a sub-viewport. Investigated directly: a `Gesture::Scroll` at deltas from -400 to -5000 at the dock's body point, and a `Drag` on the dock's own scrollbar-thumb widget, both left the "+ Add Fog" button's resolved screen position completely unchanged — the click never reached it. Traced the root cause in `window_input.rs`: `primary_mouse_wheel` has explicit branches for the inspector, timeline-tracks, and open-dropdown regions, but **no branch for either utility dock** (scene-setup or its audio-dock precedent). Neither dock's `UIEvent::Scroll` is consumed anywhere either (`rg` confirms only `dropdown.rs`/`browser_popup.rs` handle it). **This means content past ~1200px in EITHER dock is currently unreachable by any real input method in the app today** — logged as **BUG-199** (HIGH — this wave's own design explicitly invites multi-object scenes, which will commonly overflow). Out of scope to fix here (shared UI-shell wiring, not scene-setup-specific, and touching it would be scope creep into `window_input.rs`/the audio dock this design doesn't own). The flow itself was reverted to its original P1 form (no dead-code workaround shipped) and tracked as **VD-029**.

Both findings are logged in `docs/BUG_BACKLOG.md` and `docs/VERIFICATION_DEBT.md` respectively, not silently absorbed or hidden. Nothing about P5's own deliverables is implicated — the modifier-stack demo above stays green because its target rows sit above the fold.

## Deviations from brief

- One real, found-not-fixed escalation: **BUG-198** — headless `Key Z` (Cmd+Undo) never reaches `EditingService`'s undo path in this harness (only fires for a focused text field; the app-level menu shortcut lives outside `UIRoot`). Pre-existing, confirmed while building P5's own flow (which sends 4 no-op undo keystrokes after its edits — the modifier count is unchanged afterward, confirmed via a temporary assertion that failed as expected, then removed once the cause was understood). Undo correctness for all three new commands is proven at the command level instead (`execute()`/`undo()` byte-equal assertions), which is the real proof the design's own gate calls for.
- See the "real bug found" section above for BUG-199/VD-029 — a genuine, larger finding surfaced during this landing's own re-verification pass, not a P5 authorship deviation.

## Shortcuts confessed (rolled up from phase reports)

- None new beyond what P1–P4's own landing reports already confess. P5 itself: no shortcuts — all three commands, the Vm extension, and the panel UI are full implementations.

## Verification debt

- **VD-029** (new, this landing): `scene-setup-add-fog-drag.json` regressed to unreachable by BUG-199; burn-down is BUG-199's fix, not a scene-setup-panel change.
- BUG-193/194/195/198/199 all carried as OPEN, honest escalations — none silently dropped, none blocking this landing (all are either scoped-out-of-design, or proven inert at the command level where it matters).

## Click-script for Peter (≤2 minutes)

1. Drop a GLB on a layer, open Scene Setup (header "Scene" button) — the scene arrives lit and framed.
2. Rename an object, nudge its position — the card section follows the rename live.
3. Click "Add modifier:" → Twist on an object, drag the amount — watch it in the preview if reachable; the value updates live either way.
4. Add a second modifier (e.g. Bend), reorder with the ↑/↓ chips — the stack order changes.
5. **Known gap:** if the panel's content list runs long (several objects, each with modifiers), the Lights/Environment/Fog/Camera sections below may run off the bottom of the dock with no way to scroll to them today (BUG-199) — worth seeing on the rig to gauge how often real scenes hit this before scheduling the fix.
