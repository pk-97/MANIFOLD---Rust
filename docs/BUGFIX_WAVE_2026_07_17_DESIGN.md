# Bugfix Wave 2026-07-17 — six mechanical lanes over the post-GLB backlog

**Status:** APPROVED 2026-07-17 (Peter: "prepare what's needed for the mechanical Sonnet orchestration session") · directives authored by Fable 5, every root cause verified in-session against tip `9a7a7fa2` · execution: Sonnet orchestrator, overnight wave, one worktree slot per lane · Covers BUG-183/184/190/191/192/193/194/195/196/198/199 + VD-029. BUG-187-dup renumber and the PERF_BUDGET_GATE status header were fixed at this doc's own landing, not in the wave.

This is a directive pack, not an exploratory design: each lane states a verified root cause, the decided fix, the files, and the gates. Executors implement exactly what's written; anything off-script is an ESCALATE (log to the backlog, report in the landing note, do not improvise). The §2.5 audit rule doesn't apply — no lane proposes new primitives.

## 0. Orchestration contract

- One worktree slot per lane via `python3 scripts/agent-worktree.py acquire <lane> <branch>` (never per phase; `POOL FULL` = stop and surface). Step-0 guard: `git log --oneline -1` must match the intended tip.
- Lanes 1–5 are independent — run in parallel. Lane 6 is diagnosis-only and can run anytime.
- Land in batches of 2–3 lanes per the landing protocol (`.claude/GIT_TREE_DISCIPLINE.md` §2): fetch, merge `origin/main` into the lane branch, rerun the lane gate, `merge --no-ff` to main, full `cargo clippy --workspace -- -D warnings` + `cargo nextest run --workspace` + `cargo deny check bans` in the warm main checkout, push.
- Every fixed bug: flip its `**Status:**` line in `docs/BUG_BACKLOG.md` (and the summary-table row) in the same landing. That's the supersession sweep for this wave.
- Undo proofs are COMMAND-LEVEL (`execute()` + `undo()` + byte-equal graph assert) — never a headless `Key Z` step (that's BUG-198, fixed in Lane 4; until it lands, `Key Z` in a flow proves nothing).

## Lane 1 — dock scroll input (BUG-199, re-greens VD-029)

**Root cause (verified):** `crates/manifold-app/src/window_input.rs` `primary_mouse_wheel` (line ~672) branches only on `inspector_rect` / `tracks_rect` / dropdown-open. Neither dock rect is checked, so a real wheel over either dock is dropped. The docks also don't consume `UIEvent::Scroll` in their `handle_event`, which is why the headless harness's `Gesture::Scroll` was equally dead. Both panels already have working scroll plumbing: `layout.scene_setup()` / `layout.audio_setup()` rect getters (`crates/manifold-ui/src/layout.rs:211,227`), `ScenePanel::handle_scroll` (`scene_setup_panel.rs:2115`) and `AudioSetupPanel::handle_scroll` (`audio_setup_panel.rs:562`), both `ScrollContainer`-backed with `offset_content` applied at build.

**Fix (decided — ONE mechanism, the generic pipeline, not a per-panel window_input special case):**
1. In `primary_mouse_wheel`, when `layout.scene_setup().contains(pos)` or `layout.audio_setup().contains(pos)` (closed docks have zero rects, so `contains` is already the open-check), route into the UIEvent pipeline exactly like the dropdown-open branch: `self.ws.ui_root.input.process_scroll(self.cursor_pos, Vec2::new(dx, dy)); return;`.
2. In each panel's `handle_event`, consume `UIEvent::Scroll` when the position is inside `panel_rect`: call `self.handle_scroll(delta.y)`; if it returns true, return consumed with whatever rebuild signal the panel's other consumed events use.
This makes the real app and the headless harness share one path. Do NOT add inspector-style in-place scroll offsets (`try_inspector_scroll`) — the docks rebuild per frame already; measure first, optimize never (in this lane).

**Gates:**
- `scripts/ui-flows/scene-setup-add-fog-drag.json` GREEN against the azalea fixture (it is RED at tip — this is the acceptance test; it needs a `Scroll` step added before the "+ Add Fog" click since the button now sits below the fold).
- A new minimal flow proving the audio dock scrolls (open dock, Scroll, assert a below-fold widget's y moved).
- Clear VD-029 in `docs/VERIFICATION_DEBT.md` in the landing.
- `cargo clippy -p manifold-app -p manifold-ui -- -D warnings`, `cargo nextest run -p manifold-ui -p manifold-app`.

## Lane 2 — scene/automation removal commands (BUG-193 + BUG-184)

**BUG-193 (verified):** no composite removal command exists; `AddSceneObjectCommand` (`crates/manifold-editing/src/commands/graph.rs:2172`) is the shape to mirror — it snapshots the level's `(nodes, wires)` into `prev` on execute, so undo is a restore.

**Fix:** new `RemoveSceneObjectCommand` + `RemoveSceneLightCommand` in `graph.rs`, same snapshot-undo shape, each one undo unit:
- Object: delete the object's group node + its three root wires (`mesh_k`/`transform_k`/`material_k` → `render_scene`), decrement `objects`, renumber every wire whose object-port index exceeds k (`mesh_{k+1}`→`mesh_k` etc.).
- Light: delete the light node + its `light_k` wire, decrement `lights`, renumber `light_{k+1}`→`light_k` upward (single-port, no triple).
- Panel: a per-row "✕" in the Objects and Lights sections of `scene_setup_panel.rs` dispatching the new commands (follow the panel's existing PanelAction→command wiring from P2/P3).

**BUG-184 (verified):** `ClearLaneCommand` (`crates/manifold-editing/src/commands/automation.rs:306`) and `RemoveLaneCommand` (`:197`) have zero UI references. Right-clicks on the automation lane strip are explicitly unhandled today (`crates/manifold-ui/src/interaction_overlay.rs:740` — "Right-clicks are left alone").

**Fix:** right-click on an automation lane opens a two-item context menu — "Clear Automation" → `ClearLaneCommand`, "Remove Lane" → `RemoveLaneCommand` — using the existing layer context-menu infrastructure (`host.on_track_right_click` → ShowLayerContextMenu precedent at `interaction_overlay.rs:756`; lane-header affordance conventions in `docs/AUTOMATION_LANES_DESIGN.md` §7).

**Gates:** command-level unit tests for all four paths (execute + undo byte-equal; remove-middle-object renumbering; remove-only-light; clear-then-undo restores points). A headless flow proving remove-object updates the panel. Focused clippy + `cargo nextest run -p manifold-editing -p manifold-ui -p manifold-app`.

## Lane 3 — mesh stats in the graph def (BUG-194 + BUG-195, folds in BUG-196)

**Decided design (Fable, this session — do not reopen):** persist mesh stats as DECLARED node params on the two glTF mesh-source primitives. Option (b) from BUG-194's entry (pipe counts back through `ContentState`) is REJECTED — it breaks `SceneVm::from_def`'s purity contract (D3 of SCENE_SETUP_PANEL_DESIGN). Undeclared params are rejected at load (`persistence.rs` `UnknownParam`), so the params must be declared; unbound params never render on preset cards (the card surface is bindings-driven), so there's no card noise. The graph editor's raw node view will show them — acceptable, they're honest authoring facts.

**Fix:**
1. Declare on `node.gltf_mesh_source` AND `node.gltf_skinned_mesh_source` (`crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs` params block, and the skinned twin): `source_vertex_count` (Int, default -1 = unknown, range (-1, 8e6)) and `source_bbox_radius` (Float, default -1.0 = unknown). Evaluate() never reads them — document that in the ParamDef comment ("import-time provenance, read by SceneVm + merge_import_into_graph").
2. Seed both at every site that builds mesh-source nodes from a parsed asset: `build_import_graph` (`gltf_import.rs:1901`), `merge_import_into_graph`, and the skinned path (~`gltf_import.rs:857`). Vertex count and bbox radius come from the same `GltfImportSummary` / bbox data the importer already computes for recentering.
3. `SceneVm::from_def` (`crates/manifold-renderer/src/node_graph/scene_vm.rs`): add a vertex-count header field. Sum `source_vertex_count` over mesh-source nodes; for procedural mesh generators add a small closed-form table (`fn procedural_vertex_count(type_id, &params) -> Option<u32>`) for the trivially-computable ones (cube, grid, sphere-class — enumerate `Array(MeshVertex)` producers and cover the closed-form subset). Any node returning None/-1 makes the header render "≥ N" instead of "N". No fabricated numbers.
4. `merge_import_into_graph` scale-sanity (BUG-195): reference radius = max known `source_bbox_radius` across existing mesh-source nodes; keep the shipped orbit-camera-distance proxy as fallback when none is known; keep D5's 10× rule unchanged.
5. BUG-196 (same files, fold in): replace the 6 `manual_is_multiple_of` sites — `gltf_import.rs:2368, :2510, :5191, :5196, :5235`, `render_scene.rs:4493` — with `is_multiple_of`. No behavior change.

**Gates:** unit tests — import seeds both params (assert exact values on a known fixture); merge sanity picks stored radius over proxy; SceneVm "≥" vs exact logic; round-trip via `scene_setup_round_trip.rs`. `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` must be CLEAN after step 5 (that's BUG-196's acceptance). Run `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::gltf_import` (import-graph tests are feature-gated). `check_presets` after any JSON touch (there should be none).

## Lane 4 — ui-automation harness honesty (BUG-198 + BUG-192)

**BUG-198 (verified):** the headless driver's `Key` arm (`crates/manifold-app/src/ui_snapshot/script.rs:428`) only reaches `UIRoot::key_event`, which no-ops without text focus; the harness has NO undo stack at all (commands execute directly against `data.project` via `AppEditingHost` — `script.rs:629`).

**Fix:** give the Runner a real `UndoRedoManager` (manifold-editing's, same 200 cap): every command executed through the harness's `AppEditingHost` seam gets pushed; in the `Key` arm, intercept Cmd+Z / Cmd+Shift+Z and dispatch undo/redo against it before falling through to `key_event`. Any OTHER modifier-bearing Key with no seam must FAIL LOUDLY (mirror the `Text` arm's fail — `script.rs:452`) instead of returning "ok" — that kills the silent-no-op class, which is the actual bug. Plain unmodified keys keep today's `key_event` path.

**BUG-192 (verified):** `under_text_matches` (`crates/manifold-ui/src/automation.rs:367`) returns zero against `param_card.rs` flat-sibling rows even though its own documented common-ancestor semantics says they should ALL match. Prime suspect: the function indexes `nodes[p.index()]` assuming slice position == NodeId index — if the resolver's `nodes` slice diverges from id-indexing, every ancestor walk dies silently. Not yet confirmed — confirm before fixing.

**Fix (test-first, mandatory order):** (1) build a synthetic flat-sibling tree in `automation.rs`'s test module replicating param_card's real shape (label + value + slider as direct siblings under one shared content parent — the existing `under_text_walks_ancestors` test at `:503` only covers the tight-container case) and assert the query that currently returns 0 — it must FAIL before the fix; (2) step through, find the real divergence (indexing assumption first), fix `under_text_matches` to genuinely implement "nearest labeled row" — stop the shared-ancestor walk at the first node carrying ANY text so two same-parent rows don't cross-match (the doc comment at `:350` describes the intended semantics); (3) both tests green, `layer_header` case included. Optionally re-point `scripts/ui-flows/gltf-clip-scrub-retrigger.json` from its raw-widget-id workaround to `under_text` as living proof; keep the workaround if flaky.

**Gates:** the new failing-then-green test; all existing automation tests; re-run the three scene-setup flows + `gltf-clip-scrub-retrigger.json`; add ONE flow that proves Cmd+Z undoes a real mutation headlessly (e.g. add fog → Cmd+Z → assert Density row gone). Focused clippy + nextest on the two crates.

## Lane 5 — fusion coverage floor (BUG-183) — small, single-session

**Root cause (verified this session, closes the backlog entry's "unknown"):** not a partition regression. Commit `a065dec4` (2026-07-16) unbundled eight 3D-infra presets to `assets/reference-presets/`; CinematicScene was one of them and it fused (its fused-WGSL golden was deleted in the same commit). The bundled fused-preset count therefore dropped 33 → 32 while regions/atoms RATCHETED UP (measured this session at tip: 32 presets / 56 regions / 243 atoms vs the P6 floors 33/55/225). The backlog's "do NOT just lower the floor" instruction assumed a regression; the evidence overturns it — lowering the preset floor is the correct fix here.

**Fix:** in `crates/manifold-renderer/src/node_graph/freeze/proof.rs` (`fusion_coverage_baseline`, floors at ~line 2103): preset floor 33 → 32, regions floor 55 → 56, atoms floor 225 → 240 (measured 243, small churn headroom per the test's own convention). Rewrite the floor comment: cite `a065dec4` (CinematicScene left the bundled set), note regions/atoms ratcheted from post-P6 work, date it.

**Gates:** `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::freeze::proof::fusion_coverage_baseline` green (≈2s after build). Flip BUG-183's backlog status with the root cause.

## Lane 6 — diagnosis only (BUG-190 + BUG-191) — REPORT, don't fix

**Contract:** these two produce ATTRIBUTION, written into their backlog entries. No fix is attempted unless the diagnosis makes one both obvious and small (<~50 lines, mechanical, gate-able this session); anything larger is a fix-shape note for the next wave.

**BUG-190 (BrainStem ~20ms CPU-encode wall):** run `cargo xtask perf-soak tests/fixtures/gltf/khronos/BrainStem.glb --size 1920x1080 --warmup-frames 30 --profile` and read the per-node `cpu_us` breakdown to separate suspect (b) — 24× redundant `load_gltf_skinned_mesh` background re-parse — from (d) — per-frame CPU pose-sampling across 24 `node.gltf_skeleton_pose` chains. If `cpu_us` doesn't discriminate, instrument the two suspect paths with `eprintln!` timers and rerun (observe, don't deduce). Write the attribution + fix shape into the backlog entry.

**BUG-191 (perf-soak `--start` seek spike):** run `cargo xtask perf-soak "Liveschool Live Show V6 LEDS.manifold" --start 400 --profile`, attribute the single ~34-37ms post-seek frame. If it's first-use resource creation → the fix is seek-time prewarm per BUG-037's precedent (that MAY qualify as the small mechanical fix). If it's `sync_clips_to_time` seek-path cost → report only (CORE_ENGINE_MAP.md territory, one line in `sync_clips_to_time` outranks this whole wave).

## Decided — do not reopen

- Mesh stats live as declared node params (Lane 3); the ContentState pipe-back is rejected.
- Dock scroll uses the generic `UIEvent::Scroll` pipeline end-to-end (Lane 1); no per-panel window_input special case, no in-place offset optimization.
- BUG-183 is a floor update, not a regression hunt (Lane 5); the backlog's earlier "don't lower" note is superseded by the `a065dec4` evidence.
- Unmatched modifier-Key steps in the headless driver fail loudly (Lane 4); "ok"-but-no-op is the bug class being removed.
- BUG-187 stays with mesh-quantization (manifest xfail unchanged); the animation-pointer entry filed during A1 turned out to be a duplicate of BUG-170 (five `xfail:BUG-170` assets, same `gltf-json` `Target::node` serde gap) and was superseded into it — BUG-200 is a burned tombstone id. BUG-170 itself is NOT in this wave — it's real vendored-crate work, queued behind Peter's call on GLTF_ANIMATION follow-ups.
- Also not in this wave: BUG-182 (blocked on one of Peter's failing .exr files), BUG-185 (golden re-baseline = orchestrator PNG-review call at its own landing), BUG-186/BUG-188 (glTF loader polish, batch with the next conformance session), BUG-197 (already FIXED).
