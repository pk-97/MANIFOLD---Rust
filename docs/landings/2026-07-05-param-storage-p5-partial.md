# PARAM_STORAGE P5 (partial) — macro-label param names off the live manifest — landed 2026-07-05

**Branch:** wave/param-storage-p5 (merged `--no-ff` into main) · **Level reached:** L2 (unit) — this is a **partial P5 increment**, not the full phase; the design status stays "P5 in progress."

## What changed

`crates/manifold-app/src/ui_bridge/state_sync.rs` — `describe_macro_mapping`'s two `id_to_index` sites (Effect, GenParam) now resolve a param's display name from the live manifest (`fx.params` / `gp.params`.get(id).spec.name) instead of the frozen registry's `id_to_index → param_defs[i].name`. A user-added / glb-imported param was missing from `id_to_index`, so its macro-mapping label rendered "?" — the UI-display twin of the P4 Ableton/OSC blind spot. The effect's `display_name` stays a type-level template read. Unit test `describe_macro_mapping_uses_live_manifest_param_name`.

This removes `id_to_index`'s last **external** consumers (workspace grep `\.id_to_index` in `manifold-app/` → 0). It now survives only as registry-internal construction plus the unused `index_for_param` method.

## Gate

- `cargo test -p manifold-app --bins`: **131 passed; 0 failed; 2 ignored** (green on base `5832a77d`).
- `cargo clippy -p manifold-app --all-targets -- -D warnings`: clean.

## Remaining P5 (mapped, not built)

The full registry-containment field-shed is still owed — sized here so the next session starts warm:

- **Migrate the remaining positional-field UI consumers to live-manifest reads:** `param_ids` at `app.rs:1232` + `ui_snapshot/fixtures.rs` (×5); `param_defs` for param name/min/max at `state_sync.rs` (×4 remaining) + `app.rs` (×2). These read param *metadata* positionally; the live manifest's `param.spec` is the authoritative post-calibration source.
- **Then shed the fields** `id_to_index`, `param_count`, `index_for_param()`, and (once `param_ids` consumers are gone) `param_ids`. **Caveat surfaced during audit:** `param_count` is used *inside* `preset_definition_registry.rs` (~:380, a value-formatting fn) — the audit grep excluded that file, so the field-shed must trace registry-internal uses too, not just external ones.
- **Keep (boundary roles, per D2):** `param_defs` (instantiation / template seeding via `get_defaults`) and `legacy_param_aliases` (the `project.rs` one-time migration resolver). These are the "escalate rather than shed quietly" fields — they stay because a boundary role needs them.
- **Peter-owned:** the one-time real library re-save (open each show project, confirm visual + mapping integrity, save → V1.4) — P5's final proof, his call.
