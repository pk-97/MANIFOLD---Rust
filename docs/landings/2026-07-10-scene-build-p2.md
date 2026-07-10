# Scene Build + Group Params — P2 landing

**Phase:** P2 — the swap: `render_scene` ports, migration, importer · **Level reached: L2** (headless render + migration-parity PNG verified by orchestrator)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `ab215ab8` (P1) · **Worker content:** `1b33ded9` · **Orchestrator fix + docs:** `54a80448` + landing merge

## What shipped

- **`ParamSpecDef.section: Option<String>`** — `crates/manifold-core/src/effect_graph_def.rs` (serde-skip per the `is_angle` precedent; ~30 construction sites got `section: None`). Field lands here; P3 owns everything that reads it.
- **render_scene swap** — `primitives/render_scene.rs`: `rebuild()` drops the 9-param-per-object loop, adds `transform_{i}: PortType::Transform` (optional); `evaluate()` reads `ctx.inputs.transform(&format!("transform_{n}")).unwrap_or_default()` into the unchanged `model_matrix`. Node face drops from `2+9N` rows to 2.
- **Migration** — new `crates/manifold-io/src/migrations/scene_transform_v1120.rs` (recursive, group-aware): synthesizes a `node.transform_3d` per legacy object (placed inside the producing group when traced via a same-level wire, else top-level), re-points `BindingDef`s and `exposed_params`, walks `embeddedPresets`. New `1.12.0` chain rung in `migrate.rs`; `CURRENT_PROJECT_VERSION` bumped to `1.12.0` (`project.rs`); `migrate.rs`/`forward_version_guard.rs` version-asserting tests updated.
- **Importer (D9)** — `gltf_import.rs`: `node.transform_3d` seeded `pos = -center` lives inside each object group with a 4th `transform` interface output; cap now reads `render_scene::OBJECT_SLIDER_MAX` (stale `MAX_RENDER_SCENE_OBJECTS = 8` deleted — closes BUG-092); per-object card knobs get `section: <group name>`, `" 2"`-style suffixes dropped; no transform sliders on the card. Both importer tests updated (one rewritten — its original subject, per-object params on the render node, no longer exists).

## Gate (orchestrator-verified)

- `-p manifold-renderer --lib`: 1008 passed. `-p manifold-io --lib migrations::`: 25 passed (9 new). Importer tests green.
- **Round-trip** on the real `~/Downloads/meshImportTests.manifold`: loads clean at schema 1.12.0, `cam_orbit` driver survives, no legacy params remain, recentered values transferred, byte-identical across save→reload.
- **Held-out inputs**: `cc0__japanese_apricot` (4 obj) and `lowe.glb` (1 obj) both import + build clean — not the azalea the code was developed against.
- **`--features gpu-proofs`**: 1263 passed. No `.wgsl` diff (shader/uniforms untouched, as required).
- **Full workspace sweep**: 2901 passed. `cargo clippy --workspace -- -D warnings` clean.
- **Negative gates** (orchestrator re-ran): `pos_x_|rot_y_|scale_z_` in render_scene.rs → 0; `MAX_RENDER_SCENE_OBJECTS` in crates/ → 0.

## Demo (L2) — PNGs read by the orchestrator

- **Migration parity**: `/tmp/scene-build-p2/parity_pre_migration.png` (parent `ab215ab8`, param shape) vs `parity_post_migration.png` (migrated, transform-port shape) — **pixel-identical** (numpy diff max=0). Orchestrator viewed both: two blossom branches + a textured cube, indistinguishable.
- **Performer gesture**: `gesture_beat0.png` vs `gesture_beat_quarter.png` — LFO→`rot_y` on one imported object; the right-hand branch visibly swings ~86° while its sibling branch and the cube stay fixed. Orchestrator confirmed the rotation by eye.

## Orchestrator interventions beyond the worker's report

- **BUG-099 root cause + fix.** The worker found the `design_tokens` ratchet red on the P2 base and logged it as unknown-cause pre-existing drift (correct from its scope). The orchestrator identified the real cause — **P1's own `PORT_TRANSFORM_COLOR`**, masked at P1 landing because that sweep short-circuited on an inherited docs-index failure and wasn't re-run — and fixed it (baseline 200→201, `54a80448`). **Process lesson: after fixing a gate failure, re-run the full sweep; do not assume the fix is isolated when a failure short-circuited the sweep.**
- Worker also fixed BUG-092 (the import cap — genuinely in P2 scope) and left BUG-100 (fresh non-azalea import renders near-black — a lighting-default issue, out of scope) logged.

## Shortcuts taken

None on required deliverables. The round-trip test was a throwaway scratch test, deleted after (BUG-036 pattern).

## Owed / unverified

- BUG-100 (fresh non-azalea import near-black) logged, not fixed — REALTIME_3D lighting-default territory, out of P2 scope.
- Migration exercised on `meshImportTests` + the two CC0 scans; the canonical `Liveschool Live Show V6 LEDS.manifold` migration is not yet run (it has no render_scene objects to migrate, so low-risk, but unconfirmed — flagged for the wave's final landing).
