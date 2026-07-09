# Landing — PARAM_STORAGE_BOUNDARIES P2 (+ wave close)

**Date:** 2026-07-09 · **Design:** [PARAM_STORAGE_BOUNDARIES_DESIGN.md](../PARAM_STORAGE_BOUNDARIES_DESIGN.md) · **Phase:** P2 (card single-source + derive-on-save; the dual-write dies) — **final phase; wave now SHIPPED**
**Branch:** `wave/param-boundaries-p2` @ `254792c0` (+ merge `3b4c8e44` of origin/main P1+P3) → merged `--no-ff` into `main`
**Orchestrator:** Opus (X-High); **executor:** Sonnet worker (medium)

## What shipped
The calibrated param spec (min/max/name/`is_angle`) now has ONE home — the manifest (`inst.params[id].spec`), which is also what the renderer reads live. `state_sync`'s card rows are built by a single iteration over `inst.params` (both effect and generator kinds); the registry arm, the user-binding append loop, and the `meta.params` overlay block are deleted. The graph's `meta.params` shadow is now *derived from the manifest at serialize time* via the `GraphWithDerivedParams` wrapper (D12, previously unimplemented) — wired into both serialize arms. `EditParamMappingCommand`'s dual-write to `meta.params` is gone; the sole spec write is to the manifest (scale/offset still land on the graph `BindingDef`, their only home).

## Gate (orchestrator re-ran on the merged branch — P1+P3 already in)
- `cargo test -p manifold-editing -p manifold-app -p manifold-core`: **all green** — 162 app, 334 core, 99+67+34+6 editing, plus the reconcile/reload/e2e suites.
- Regression `calibrated_param_derives_meta_params_on_save_not_the_stale_shadow` (manifold-core): **green** — a calibrated manifest spec reaches the wire via `GraphWithDerivedParams` (JSON byte-comparison against the manifest spec), while a graph literal seeded with stale template values is never re-read.
- `cargo clippy --workspace -- -D warnings`: **exit 0**.
- Negative — `meta.params` WRITE sites in `manifold-editing/src/commands/effects.rs`: **zero** (the one survivor at :1158 is a test-helper `.iter()` read, blessed). Overlay block `single reshape source` in `state_sync.rs`: **zero**.

## Verification level: **L3 for the card half, L1 for the calibration-drag gesture** (target L3)
The worker authored `scripts/ui-flows/calibrated-param-card-reads-manifest.json` and it ran (orchestrator re-runs at land, below): the inspector renders Mirror/Bloom/Strobe cards with real names and manifest-sourced ranges. The literal "drag a calibrated slider → reload → see the real degree range" gesture is **not** L3-provable — it lives in the graph-editor mapping popover, which the `ui-snap --script` harness does not accept as a scene (whitelist is timeline/states/inspector/paramsteps/… — the graph editor is dump-only). That round-trip is proven at the Rust level by the regression test instead. **Gap → VD-020** (UI_AUTOMATION harness must gain the graph-editor scene before it can reach L3).

## Follow-ups opened
- **BUG-078 (LOW):** three `meta.params` shadow-readers left untouched (correctly — brief rule). Two are D4-blessed (graph-editor popover `full_reshape_from_def`; Save/Export `preset_source_def`). The third — the renderer's `PresetRuntime` `param_reshape`, rebuilt on a `graph_structure_version` bump — reads the in-memory graph `meta.params`, which post-P2 is stale until serialize. Bounded, non-data-loss (renderer operates on a read-only `Arc<Project>` snapshot; the authoritative manifest is untouched): a calibrate-then-structural-edit-before-save sequence could momentarily reshape the *rendered* param mapping from the stale shadow until the next content→UI sync. Fix shape: have `param_reshape` read the manifest, or derive `meta.params` before the reshape.
- **BUG-077 (LOW, pre-existing, external):** 6 `ui_cache_manager` tests red on main since `0bb51dad` — trunk default sweep is red until fixed. Not this wave.

## Peter click-script (~2 min)
1. Calibrate a param (drag its range in the mapping drawer), Save, reload the project, reopen the card. → Expect: the card shows your calibrated range, not the template default.
2. Expose a user param, calibrate it, Save/reload. → Expect: range persists; one home, no drift.

## Deviations from the brief
- Two pre-existing unit tests (`edit_stock_param_seeds_graph_and_roundtrips`, `edit_gen_param_seeds_graph_and_roundtrips`) asserted the deleted dual-write and were rewritten to assert against the manifest (required update, expected values re-derived from the seeded specs).
- The three shadow-readers were documented in the doc's P2 section and BUG-078, not adapted around (correct escalation discipline).

## Status line (quoted verbatim — wave close)
`**Status:** SHIPPED — P1 (\`0438b60e\`) + P2 (\`254792c0\`) + P3 (\`eec807cd\`) all landed on main 2026-07-09 …`
