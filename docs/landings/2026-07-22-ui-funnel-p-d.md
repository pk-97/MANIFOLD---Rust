# Landing: UI_FUNNEL_DECOMPOSITION P-D — intent-enum decomposition (flat 12)

**Date:** 2026-07-22 · **Branch:** `lane/ws1-intents` → main · **Executors:** ws1-dispatch-pd (Opus dispatcher teammate) + one Opus lane (D-D1) + in-context computable audits (D-D0/D-D3, endorsed deviation). **Lander:** ws1 orchestrator (Fable top session).

## What landed
- `PanelAction` (303 variants) → FLAT sum of 12 per-domain intent enums in `panels/actions.rs` (`Transport/Editing/Layer/Marker/Project/Browser/Clip/Params/Modulation/Mapping/AudioSetup/Root`), variant bodies VERBATIM, 12 `From` impls, ~783 emit/pattern sites wrapped (`c233207c` — in `.git-blame-ignore-revs`).
- Router `ui_bridge::dispatch` = 12-arm exhaustive sum match; `dispatch_inspector` chain + `dispatch_chain_completeness` invariant DELETED (superseded by compiler-proven routing totality — amended D5, recorded in the design doc).
- D-D3 scrub-trio kill list (15 full + 3 reconstructed trios) appended to `pd-partition.md` — P-I's pre-derived target.
- D5 amendment applied verbatim; D-D2 struck (no deviations to normalize).

## Gates (dispatcher's independent rerun + lander's full sweep below)
- Census: 303/303 across the 12 enums, disjoint, 0 missing/extra/dup (script, compiler-independent).
- Oracles `undo_baseline`/`mapping_undo_baseline`/`bug_266_tab_pin`: green, files UNTOUCHED by the phase (empty diff — assertions provably unmodified).
- Scoped clippy/nextest: clean; 1172/0/3. 3-crate scope proven: ui, app, renderer (test target compiles); editing/playback reference the type only in comments (cannot depend on ui).
- Full-workspace sweep + flow suite: quoted in the push-time addendum below.

## Verification level
L1 + L3 (flow suite at landing). Behavior identical by construction (verbatim bodies, one wire type); the flow suite exercises the re-parented wire end-to-end.
