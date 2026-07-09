# Landing — PARAM_STORAGE_BOUNDARIES P1

**Date:** 2026-07-09 · **Design:** [PARAM_STORAGE_BOUNDARIES_DESIGN.md](../PARAM_STORAGE_BOUNDARIES_DESIGN.md) · **Phase:** P1 (reconcile stage + delete the ordering machinery)
**Branch:** `wave/param-boundaries-p1` @ `0438b60e` → merged `--no-ff` into `main`
**Orchestrator:** Opus (X-High); **executor:** Sonnet worker (medium)

## What shipped
Load-time param resolution is now correct **by construction** instead of by ordering discipline. `PresetInstance` stashes its raw V1.4 wire map (`pending_wire`); the loader deserializes, installs the embedded-preset overlay, then calls `Project::reconcile_param_manifests()` to rebuild every instance's manifest against the completed registry. The 2026-07-06 defensive machinery that guarded the old fragile ordering is deleted:
- `EmbeddedPresetsPrePass` + the JSON pre-scan (`crates/manifold-io/src/loader.rs`)
- The rollback API — `ProjectPresetsSnapshot`, `project_presets_snapshot()`, `restore_project_presets()` (`crates/manifold-renderer/src/preset_loader.rs`) + its 3 call sites (`project_io.rs`, `app_lifecycle.rs` ×2) + the `overlay_snapshot_restores_after_a_candidate_install` test.

`reconcile_manifest()` keeps the stash parked (does **not** clear `pending_wire`) when the template still isn't resolvable — the keep-don't-drop retry that preserves BUG-036-class values for a later reconcile once the registry gains the definition.

## Gate (orchestrator re-ran in the worktree; not solely the executor's claim)
- `cargo test -p manifold-core -p manifold-io -p manifold-app`: **all green** — 162 app, 333 core (incl. `reconcile_*` identity tests), 37 io, 10 legacy_param_id_resolution, 3 user_param_bindings_e2e, 15 load_project (incl. `load_liveschool_live_show_v6`), project_preset_overlay 5 (rollback test correctly gone).
- 3-arm `project_local_preset_reload` (arm 3 = order-independence: bare `load_project_from_json` → install overlay → `reconcile_param_manifests()` directly → full resolution): **green**.
- `cargo clippy --workspace -- -D warnings`: **exit 0** (only pre-existing manifold-media Obj-C SDK deprecation notes).
- Negative grep `EmbeddedPresetsPrePass|project_presets_snapshot|restore_project_presets`: **zero hits**.

## Verification level: **L2**
Acceptance artifact read: round-trip load of the real `~/Downloads/meshImportTests.manifold` (a live BUG-036 specimen — its saved `gen_params.params` serializes as `{}`). Post-P1: all **17** template params resolve, the `cam_orbit` driver binds to a real param, and the log carries **zero** "dropping unknown param id" and zero placeholder "keeping param" lines — full resolution via the reconcile path, not the informed-drop backstop. Target for P1 was L1 (no visible demo); **no verification debt** — level reached exceeds target.

## Peter click-script (~1 min — this is an invisibility check; nothing new should appear)
1. Open a project containing imported generators (e.g. a mesh-import scene). → Expect: all params present on the cards, exactly as before P1.
2. Confirm any generator with a bound driver (camera orbit / audio) still drives. → Expect: unchanged behaviour.
The point of P1 is that loading, snapshot-restore, and calibrated ranges behave **identically** — it removes the ways they could silently regress, it adds nothing visible.

## Deviations from the brief
1. `pending_wire` uses no `#[serde(skip)]` (the doc §3 snippet assumed a derived impl; `PresetInstance` hand-writes `Serialize`/`Deserialize`, so the field is simply never written by the `Serialize` impl — same "never on the wire" property, and the attribute wouldn't compile there).
2. `reconcile_manifest()` clear-condition (`template_known_for` gate) is an interior detail the §3 snippet didn't spell out; it's the only reading consistent with the doc's idempotence language and the required order-independence test arm.
3. Re-derivation guard found **4** app call sites of the `_with` loader fns, not the audited 3 — the extra is `ui_snapshot/fixtures.rs:69` (the headless `ui-snap` harness). D3 keeps the installer signature identical, so it needs zero behaviour change; only its stale comment (naming the deleted pre-deserialize hook) was corrected. Noted so the audit count is known-one-short for P2/P3 planning.

## Status line (quoted verbatim, bumped in the same merge)
`**Status:** IN PROGRESS · P1 SHIPPED (\`wave/param-boundaries-p1\`) · P2–P3 not built · 2026-07-06 · Fable`

## Next
P2 (card single-source + derive-on-save) and P3 (migration reads `embeddedPresets`, BUG-040) are unblocked and independent — fan out in parallel off this merged tip.
