# Landing report — lane/scene-bugfixes (BUG-218 + BUG-212) · 2026-07-17

**Landed:** `67e606ca` on main. Orchestrated Sonnet lane (slot-1), Fable orchestrating.

## What landed

- **BUG-218 (HIGH)** — `InsertMeshModifierCommand`/`RemoveMeshModifierCommand`/`MoveMeshModifierCommand` (`crates/manifold-editing/src/commands/graph.rs`) now resolve the modifier-chain splice point for BOTH committed D12-era document shapes: the import shape (`node.scene_object` inside the group; walk its `vertices` input) and the migrated/starter shape (`migrate_scene_object_wires`' root-level scene_object; walk `system.group_output`'s `vertices` port — the pre-existing behavior). Shape resolution mirrors scene_vm.rs:617's duality handling. Inverse-pair tests for both shapes in manifold-editing.
- **BUG-212 (MED)** — `DuplicateSceneObjectCommand` clones `string_bindings` entries (the "Model File" path bindings) re-targeted at the clone's fresh NodeIds, with whole-vec undo (`prev_string_bindings`). Imported objects' duplicates now load geometry. Card exposes (`bindings`/`exposed_params`) stay excluded per SCENE_OBJECT_AND_PANEL_V2 D11 — deliberate.

## Escape analysis (during this landing, caught before push)

The first BUG-218 fix handled only the import shape and broke modifier insert on every SceneStarter-shaped scene. Caught by the landing sweep's `manifold-app::ui_bridge::project::tests::insert_modifier_on_scene_starter_lands_in_the_object_group_body` — the lane's focused gate covered manifold-editing + manifold-renderer but the consumer test lives in manifold-app. Lesson applied: modifier-command lanes gate on all three crates now. `Escaped:` lines are on both backlog entries.

Process note (confessed by the lane): the branch's two commits are mislabeled — the BUG-212-labeled commit carries both bugs' Rust (shared file + pathspec auto-staging), the BUG-218-labeled one is docs-only. Commit messages document it; backlog Status lines are the authoritative record.

## Gates (final, at landing)

- Full workspace sweep, warm main checkout: clippy `--workspace -D warnings` clean · `cargo deny check bans` ok · `cargo nextest run --workspace` **3634/3634 passed** (1 pre-existing "leaky" heuristic flag on `freeze_has_no_leaks`, known false-positive).
- GPU proof: `duplicate_demo_pair_renders_original_then_original_plus_offset_copy` (gpu-proofs, plain cargo test) green with the manual path-copy workaround REMOVED — the test now proves the shipped mechanism.
- L3 flow: `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` — 22/22 steps ok; modifier rows appear and undo restores (previously a silent no-op). Orchestrator reviewed the PNGs (Twist/Bend stacks with Axis/Angle/Center rows, reorder + remove controls, against the real GLB fixture).

**Verification level reached: L3** (scripted flow + reviewed renders). No new verification debt.

## Click-script for Peter (~1 min)

1. Open a project with any imported GLB scene layer → open Scene Setup.
2. Select an object row → click "Twist" under Add modifier. Expect: a Twist stack (Axis/Angle/Center) appears immediately in Modifiers.
3. Cmd+Z. Expect: the stack disappears.
4. Select the object → Duplicate (header button). Expect: the clone RENDERS (offset copy), not an invisible object.
5. Repeat 2 on a fresh SceneStarter layer's cube. Expect: same behavior (this was the shape the first fix broke).

## Status updates in this landing

- `docs/BUG_BACKLOG.md`: BUG-218, BUG-212 → FIXED 2026-07-17 with mechanisms + Escaped lines.
