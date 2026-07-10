# Scene Build + Group Params — P5 landing (WAVE COMPLETE)

**Phase:** P5 — "+ Object" / "+ Light" gestures + same-pair wire ribbons (D7/D7a/D8) · **Level reached: L2 + gesture** (faithful editor capture of both gestures on the real import; L4 feel-pass owed to Peter)
**Orchestrator:** Opus · **Worker:** Sonnet · **This closes the SCENE_BUILD_AND_GROUP_PARAMS wave (P1–P5 all shipped 2026-07-10).**
**Base:** `9cf953c6` (P4) · merged `origin/main` `1fbaa681` (audio-dock P3b + build-speed).

## What shipped

- **`AddSceneObjectCommand`** (`manifold-editing/commands/graph.rs:2063`) + a node-face **"+ Object"** button (new `NodeRow::Action` variant, `model.rs:77`). One undoable command: bump `objects`, create an "Object N" group (`generate_cube_mesh` + `phong_material` with a cycled tint + `transform_3d`), wire the three outputs to `mesh_k`/`material_k`/`transform_k`. Inverse-pair undo (`assert_eq!(def, &before)`).
- **`AddSceneLightCommand`** (`graph.rs:2229`) + a **"+ Light"** button per D7a. Bumps `lights`, spawns a **bare** `node.light` (no group), named "Light N", auto-wired into `light_k`. Defaults: Sun, white, intensity 1.0, `pos (0,7,7)` (~45° elevation — the primitive's default is straight-overhead), **`cast_shadows` ON** (inert until REALTIME_3D P2). Inverse-pair undo.
- **Same-pair wire ribbons (D8)** — `graph_canvas/render.rs:1302` `draw_wire_ribbon` + `model.rs:485` `group_wires_by_pair`: ≥2 wires between one (source,dest) pair draw as one ribbon with an `×N` badge; per-node focus tiering expands on hover/selection. Feedback-wire styling, hover-dim, arc routing preserved.
- Canvas emits context-free `GraphEditCommand::AddSceneObject`/`AddSceneLight` (`graph_edit.rs`); `app_render.rs:2695,2717` translate to the `EditingService` commands (same pattern P4 established).

## Gate (orchestrator-verified on the merged tree)

- Command tests (inverse-pair, full undo restore) + canvas tests (ribbon collapse, action rows on render_scene only, singletons unaffected) green.
- **Full `cargo test --workspace`: 65 binaries, 0 failures.** `cargo clippy --workspace -- -D warnings`: clean.
- Negative gate: `rg "virtual.*socket|auto_grow" manifold-ui` → 0 (deferred auto-grow didn't sneak in).

## Demo (L2 + gesture) — read on the REAL azalea import

- `/tmp/scene-build-p5/gltfeditor-addobject.after.png` (+ crop) — after one "+ Object" click: a new tinted "Object 3" group box (cube+material+transform) wired into the render node's new ports; tree-dump **nodes 8→9, wires 12→15**; the three new wires draw as one D8 ribbon. Orchestrator read it.
- `gltfeditor-addlight.after.png` (+ crop) — after one "+ Light" click: `Lights: 2`, a bare "Light" node wired to `light_1`; tree-dump **nodes 8→9, wires 12→13**. Orchestrator read it.

## First-consumer harness finding (logged)

The editor scene's preview pane is content-thread-bound and can't render headless, and generator node-output *pixels* (the actual placeholder cube) aren't provable through `ui-snap` (`GeneratorRenderer` isn't wired into it). Logged in `docs/UI_HARNESS_UNIFICATION_DESIGN.md` Deferred. The gesture is proven at the canvas/structure level (real command → real graph mutation → real render seam) + tree-dump counts, NOT at the rendered-cube level — an honest boundary, not a fake.

## Merge-resolution (greened main — audio-dock P3b scoped-gate slips, not P5 defects)

- design-token ratchets: P3b's `audio_trigger_section.rs` added 3 raw `Color32::new` (color baseline 187→190) and 2 raw `corner_radius: 2.0` (tokenized to `color::SMALL_RADIUS`, radius guard stays absolute-0). Both were **red on origin/main** — P3b's scoped gate skipped `design_tokens`.
- BUG-105 logged: `audio_mixdown` analysis-only test is order-flaky in the full sweep (passes in isolation + the full `-p manifold-playback` suite); P5 touches no playback code.

## Owed — Peter's L4 feel-pass (not headless-verifiable)

≤2-minute click-script for the rig:
1. Open a scene with a `render_scene` node (a glTF import, or a fresh 3D generator).
2. Click **"+ Object"** once on the Render Scene node face → a new tinted box with a cube appears, wired in; preview re-composes with an extra cube.
3. Click **"+ Light"** once → a bare "Light" node appears beside Render Scene, wired to the new slot; preview visibly re-lights.
4. Undo twice → both gestures fully reverse (boxes/wires/counts restore).

## REALTIME_3D §8 cross-refs (verified still true at wave close)

The SCENE_BUILD §8 amendments to REALTIME_3D hold: object port group = `mesh_n`+`material_n`+`base_color_map_n`+`transform_n: Transform` (P2 built it); gizmos-write-`transform_3d`-params / gizmo-target-follows-`transform_n`-wire is still the accurate P6 promise; REALTIME_3D P6's entry (SCENE_BUILD P2 landed) is satisfied. P3/P4/P5 did not touch the transform model.
