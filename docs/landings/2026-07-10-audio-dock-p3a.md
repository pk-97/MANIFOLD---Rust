# Audio Setup Dock & Trigger Unification — P3a landing (P3 split)

**Phase:** P3a — Triggers-matrix deletion + shared-drawer Length-row capability (the unblocked half of P3) · **Level reached: L2** (inspector-scene PNG + tree dump, orchestrator-read: the fire-mode drawers render unchanged after the refactor)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `470228ec` · **Worker content:** `47f2a112` · landing merge on `main`

## Why P3 split

The worker hit a real structural finding and correctly STOPPED (per its brief's escalation rule) rather than shimming: the design's layer-side authoring section can't live in the timeline track header. `layer_header.rs`'s row height is the fixed `color::TRACK_HEIGHT` constant, with a test (`expanded_audio_content_fits_normal_track_height`) asserting every control fits it "without a per-type exception." A **variable-length** list of clip triggers, each expandable to a full Source/Feature/Band/Amount/Attack/Release/Length drawer, cannot fit that fixed budget without either variable row height (which ripples into the `single-source-y-layout` + `track-header-invariant` invariants) or a different surface — a placement the design doc left open ("layer header/inspector"). That is a design decision, escalated to Peter, not a worker call. P3 therefore splits:

- **P3a (this landing):** matrix deletion + Length-row capability — both independent of the placement question, both gate-clean.
- **P3b (pending Peter's placement decision):** layer-side authoring UI + fire meter (D6) + BUG-082 fix + state_sync view-model rows.

## What shipped (P3a)

- **Audio Setup Triggers matrix fully deleted** (compiler-driven): `build_trigger_section`, `TriggerRouteRow`, `TriggerRowIds`, `trigger_band_color`, `trigger_swatch_style`, `dim_color`, `update_trigger_levels`, `sensitivity_drag_target`, `CalibrationDrag::Sensitivity`, `TrigControl`, ~8 `PanelAction` variants, `SetAudioSendTriggersCommand`, and `AudioSend::{triggers_with_route, has_active_triggers, trigger_for}`. The Consumers section survives and is re-pointed to a new `Project::clip_trigger_consumers` (`project.rs:1420`, mirrors `audio_mod_consumers`) walking `layer.clip_triggers` — so existing clip triggers still DISPLAY (navigational, click-to-owning-layer) and still FIRE (P2 evaluator, untouched).
- **Length-row capability in the shared drawer** (D5, dormant): `build_audio_mod_drawer` gained `length_beats: Option<f32>`; both existing callers pass `None` (behavior byte-identical). `LENGTH_OPTIONS`/`length_labels`/`length_option_index`/`format_beats` added to `param_slider_shared.rs`. The row is wired but has no caller until P3b's clip-trigger drawer.

## Interim state (honest)

After this lands, existing clip triggers (migrated from real projects in P2) **fire and display** normally, but a clip trigger **cannot be authored/edited via UI** until P3b lands the layer-side surface. This is acceptable because authoring happens at set-prep (not mid-show) and P3b is next in the wave — and it's strictly better than keeping the dead matrix alive (D2's explicitly-rejected "matrix as editor"). Flagged to Peter at landing.

## Gate (orchestrator-verified, independently re-run)

- `-p manifold-core --lib`: **349 passed** (+2 `clip_trigger_consumers` tests). `-p manifold-editing --lib`: **101** (−1 deleted matrix test). `-p manifold-ui --lib`: **655** (+3 Length-row tests). All 0 failed — re-run by orchestrator, not worker-reported.
- `-p manifold-app` build clean; `clippy -p manifold-core -p manifold-ui -p manifold-app -p manifold-editing --features manifold-app/ui-snapshot -- -D warnings` clean.
- **Negative gates (orchestrator re-ran):** `build_audio_trigger|clip_trigger_drawer` → **0**; matrix fns in `audio_setup_panel.rs` → **0**; `SetAudioSendTriggersCommand` → **0**.

## Demo (L2)

`target/ui-snapshots/inspector/inspector.png` + `.tree.json` — orchestrator-read. The GLOW layer's Bloom and Strobe effect cards render their fire-mode audio-mod drawers (Source/Feature/Band/Amount/Attack/Release + Mode) **unchanged** after the Length-row addition and the matrix deletion. This proves the shared drawer still serves its two existing callers byte-identically. No L3 flow — the layer-side authoring surface the flow would drive is P3b (blocked on placement).

## Not done / owed to P3b

Layer-side AUDIO authoring section (placement decision pending), fire meter / D6, BUG-082 fix (stays **OPEN** — correctly not marked fixed, since the meter is its fix and the meter is P3b), state_sync view-model extension. The `update_trigger_levels` per-frame pattern P3a deleted is the closest precedent for the D6 meter's content-thread→UI live-value plumbing.

## Shortcuts

None beyond the documented scope-cut. BUG-082 left OPEN (not falsely marked fixed).
