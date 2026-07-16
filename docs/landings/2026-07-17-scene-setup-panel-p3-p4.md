# SCENE_SETUP_PANEL_DESIGN.md P3+P4 — landed 2026-07-17 @ 658a2acc (flow fix follow-up @ 6c7d47fc)

**Branch:** wave/scene-setup-panel · **Level reached:** L3 (scripted UI-flow interaction, incl. a real held-out asset merge run and read by the orchestrating session) / target L3 (§10)
**Doc status line (quoted verbatim):** IN PROGRESS — P1 (column + discovery + Environment/Fog) + P2 (Objects section) + P3 (Lights + Camera sections) + P4 (Import Model merge, held-out warehouse+skull gate passed) SHIPPED 2026-07-17; P5 (modifier stack) not implemented. Sonnet-executable, orchestrated overnight. BUG-193 (no object/light remove command), BUG-194 (vertex count not computable from def), BUG-195 (merge scale-sanity has no stored object radius, defaulted proxy) opened as honest escalations, not blocking. · 2026-07-16 · Fable 5 (design session with Peter)

## Gate results (verbatim)

All gates re-run independently by the orchestrating session (per DESIGN_DOC_STANDARD §8.5), including a genuine held-out-asset gate the executing worker for P4 could not run itself (its worktree lacks the gitignored fixtures).

`cargo build --workspace` (main checkout, post-merge): clean, 43.19s.

`cargo clippy --workspace -- -D warnings` (main checkout, post-merge): clean (2 pre-existing unrelated manifold-media ObjC deprecation warnings).

`cargo nextest run --workspace` (main checkout, post-merge, final re-run after the flow fix below): `3526 tests run: 3526 passed, 12 skipped`.

`cargo deny check bans`: `bans ok`.

Negative gates (§4): `rg "MutateProject|Arc<Mutex|Arc<RwLock" crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0 hits; `rg "Project\b" crates/manifold-renderer/src/node_graph/scene_vm.rs` → 0 hits; `layer.rs`/`ports.rs` untouched (`git diff --stat` empty against the P2 landing tip). Merge-path negative gate: `rg -n "assemble_import_graph"` inside `merge_import_into_graph`'s body → 0 hits (the merge never calls the whole-graph assembler).

**P4's held-out gate — run for real, by the orchestrator, in the main checkout** (the worktree lacks these fixtures): a new test `merges_skull_into_warehouse_held_out_real_assets` (authored by the orchestrator, `crates/manifold-renderer/tests/scene_setup_p4_heldout_merge.rs`) imports `abandoned_warehouse_-_interior_scene.glb` (35 objects), merges `skull_salazar_downloadable.glb` into it via the real production parse path, and asserts: object count grows (35→37), zero chrome node type-ids among the new nodes, and the D5 scale-sanity rule fires correctly — `report_lines: ["merged import scaled ×13.3908 to match the scene (incoming radius 1.3800 vs scene reference 18.4789)"]`, i.e. the skull (tiny relative to the warehouse) was scaled UP by the correct factor. The merged def was then validated: `graph-tool validate … --kind generator` → `OK`; `graph-tool fusion` → clean, all new nodes correctly classified as `boundary:io_bridge`/`boundary:non_gpu`. This test is now part of the default `cargo nextest run --workspace` sweep (it ran as test #3526 above) — it will only pass on a machine that has these three fixtures on disk, matching the codebase's existing convention for other large-glb-fixture tests (e.g. the azalea-fixture tests already in `gltf_import.rs`), which are likewise unconditional and un-ignored.

L3 flows, re-run by the orchestrator in the MAIN checkout:
- `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-light-intensity-drag.json` — 9/9 steps ok.
- `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-light-cast-shadows-toggle.json` — **failed on first re-run** (see Deviations), fixed, then 6/6 steps ok on re-verification.

PNGs read by the orchestrator (affordance check): full panel with Lights section (Mode/Color/Intensity/Position/Aim/Cast Shadows/Shadow Softness/Light Size, all distinct clickable chips) and Camera section (Orbit/Tilt/Distance/FOV/Lens) live on the real azalea import; before/after PNG pair of the Cast Shadows toggle, which also live-updates the header's "shadow caster" count (1 → 0) — a nice proof the Vm and the panel are genuinely wired end to end, not just independently plausible.

## Deviations from brief

- **A real regression, caught and fixed before landing, not shipped red.** P4 added a new "Import Model…" button to the Objects section, which shifted every row below it (the entire Lights/Environment/Fog/Camera sections) down by exactly one row-height (24px). P3's `scene-setup-light-cast-shadows-toggle.json` flow clicked a raw pixel coordinate for the Cast Shadows "−" stepper; that coordinate no longer landed on the right widget once P4 landed on top of P3 in the same worktree. Caught by the orchestrator's own re-run in the main checkout (not by either worker's self-report — both workers' individual runs were against different intermediate states of the panel and never exercised the fully-merged P1–P4 layout together). Diagnosed via a forced-failure `--dump` of the live widget tree, which showed the target row's actual rect (`[812, 768, 22, 24]` for the "−" button) was 24px below where the P3-era coordinate pointed. Fixed by repointing the click to the correct pixel position; verified 6/6 in both the worktree and the main checkout after the fix.
- **P4 defaulted an undecided proxy rather than fabricating or blocking.** D5's scale-sanity rule needs "the scene's reference radius (the largest existing object's)," but no per-object bbox/size metadata exists anywhere in the graph def (the same class of gap BUG-193/194 found for counts). P4 defaults to inverting the target's own `node.orbit_camera.distance` through the importer's own `distance = 2.2 * radius` formula, skipping normalization entirely when no such camera exists. Logged as **BUG-195**, not blocking — the held-out gate above proves the proxy produces a sane, correctly-directioned result on a real asset pair.
- **A confessed, unfixed interaction gap:** an incoming object whose glTF animation drives scale gets its seeded normalize `scale_x/y/z` silently overridden at runtime by the animation's port-shadow wire (port-shadows always win over static params). Named in the merge function's own doc comment, not hidden; not fixed this phase (narrow case: merging a >10×-mismatched, scale-animated asset).

## Shortcuts confessed (rolled up from phase reports)

- P3: no dedicated P3-specific round-trip test — Lights/Camera params write through the identical `SetGraphNodeParamCommand` path P1's fog round-trip test already proves persists; P3 adds no new serialization shape, only new addresses into already-covered params. The `light_size` Contact-only sub-row is always rendered live (a parameter dependency, not conditional UI) with an indent, not a dimmed/disabled treatment.
- P4: see the BUG-195 escalation above (scale-sanity proxy) and the scale-animation interaction gap above. Both are named, not hidden.

## Verification debt

None newly opened. BUG-193/194/195 are tracked escalations (per the doc's own §8 contract), not silent verification gaps. The flow-fix regression above was caught and closed within this same landing, not carried forward.

## Click-script for Peter (≤2 minutes)

1. Open Scene Setup on the real azalea (or any imported glTF) layer. Scroll to **Lights** — expect: Mode/Color/Intensity/Position/Aim/Cast Shadows/Shadow Softness/Light Size rows, all live steppers.
2. Click the Cast Shadows "−" to toggle it off — expect: the value reads "Off" AND the header's shadow-caster count drops by one, live.
3. Scroll to **Camera** — expect: Orbit/Tilt/Distance/FOV rows matching the imported camera's actual framing, plus a **Lens** sub-section (Focus Distance/F-Stop/Shutter Angle/Exposure) if the importer inserted a physical-lens stage.
4. In **Objects**, click **Import Model…** and pick a second `.glb` — expect: its objects appear merged into the SAME scene (not a new layer), correctly scaled to sit believably alongside the existing content, with no duplicate camera/lights/fog.
5. Undo — expect: the entire merge disappears as one step.
