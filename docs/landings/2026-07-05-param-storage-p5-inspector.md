# PARAM_STORAGE P5 — inspector single-source (is_angle on manifest spec) — landed 2026-07-05

**Branch:** wave/param-storage-p5-inspector (merged `--no-ff` into main @ `bdeebbd3`) · **Level reached:** L2 (full workspace test + clippy sweep). The final P5 code increment, after the P5-partial macro-label slice and the P5 field-shed.

## What changed

The angle-display flag `is_angle` gets a single persistent home on the manifest param spec, closing the last P5 code gap. Since the P2 param-storage unification dropped `is_angle` from the unified param shape, `synth_user_binding` rebuilt it as a hardcoded `false` on every card, so **no exposed angle param ever showed degrees** (the display feature — `format_param_value`'s `val.to_degrees()` at `param_slider_shared.rs:542`, wired through `ParamInfo.is_angle` — was fully built but starved of a true value).

- **`effect_graph_def.rs`** — `ParamSpecDef` gains `pub is_angle: bool` with `#[serde(default, skip_serializing_if = "is_false")]`. Back-compat: old files load (default false); non-angle presets stay byte-identical (skipped when false); the V1.4 migration only writes value-state (`value`/`base`/`exposed`), never the spec, so no migration change was needed.
- **`effects.rs`** — seeded at every seed point: `append_user_binding` and the position-aware `insert` (undo-restore) set `is_angle: binding.is_angle` (captured from the inner `ParamType::Angle` at expose). `to_spec` (registry/template) and `spec_from_binding` (pre-spec fallback) default `false` — bundled params carry no angle source and use `format_string` for degrees. `synth_user_binding` now reads `spec.map(|s| s.is_angle)` instead of the hardcoded `false`.
- **`state_sync.rs`** — the inspector card overlay (the existing single-source mechanism that already carries calibrated min/max/name/whole_numbers/value_labels) now also carries `row.is_angle = spec.is_angle`, so the manifest/graph spec is the single display source.
- **`gltf_import.rs`** — `card_param` threads a new `is_angle: bool` param; `cam_orbit` / `cam_tilt` / `cam_fov` (the documented DEG2RAD angle sliders) pass `true`, every other import slider `false`.
- **Mechanical sweep** — adding the field broke every non-spread `ParamSpecDef { … }` literal (E0063). ~30 test-fixture literals across 16 files got `is_angle: false`; delegated to a Sonnet agent from the compile-error list, verified independently (workspace check + a diff audit confirming every added line is `false` except the four semantic sites handled by hand).

## Two corrections to the earlier plan (found by reading the code first)

1. The memory/design note said a *calibrated bundled param shows its uncalibrated range in the card* and prescribed migrating the card off `param_defs`. That was **already solved**: `EditParamMappingCommand` dual-writes calibration to both the live manifest spec AND the `meta.params` shadow (`effects.rs` execute), and the card overlay reads the shadow — so calibrated ranges already displayed. The only real defect was `is_angle`.
2. The note said to add `is_angle` **and `convert`** to the spec. `convert` was **not** added — it already lives correctly on `BindingDef` (synth reads it; the card derives `whole_numbers` onto the spec at expose via `matches!(convert, IntRound | EnumRound | Trigger)`). Putting it on the spec would have duplicated a field with a home.

## Gate (run independently of the implementing agent)

- `cargo test --workspace --no-fail-fast`: only failure is `manifold-ui design_tokens::no_new_raw_color_literals` (**BUG-030**, 201/200, pre-existing, fails identically on main; this change touches no manifold-ui color literals). The 992-test manifold-core suite and every other crate pass. New test `user_exposed_angle_param_carries_is_angle_through_manifest_and_synth` proves seed + JSON round-trip (`"isAngle":true` survives) + synth read-back.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean (only the pre-existing manifold-media `cc` ObjC-deprecation warnings, which don't gate).
- Re-gated after merging current `origin/main` (which had advanced `add6c588 → 4e174637`, touching `node_graph/` and `param_card.rs`) into the branch: merge clean, workspace test + clippy still green, `is_angle` test still passes.

## Verification debt

- **VD-010 (new):** the degree readout on an exposed angle param's inspector card is proven by unit test through the manifest + synth, but not observed in a running app (headless can't drive the inspector render). Owed: expose an angle inner param in the graph editor, confirm the card shows `NN°` and edits round-trip radians↔degrees.

## Remaining P5

- **Library re-save to V1.4** — the one-time real-project re-save, Peter-owned (open each show project, confirm integrity, save). P5's final proof; the code side is now complete.
