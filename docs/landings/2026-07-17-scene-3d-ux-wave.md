# Landing report — scene/3D/import wave · 2026-07-17 (consolidated)

Fable-orchestrated wave (Sonnet lanes, per-lane PNG review by the orchestrator). This report consolidates the day's landings that shipped without individual reports — a protocol slip during the highest-throughput stretch, confessed here rather than backfilled with fiction; per-landing gate output lives in the merge commits and the design docs' status lines. Individual reports already exist for lane/scene-bugfixes and the panel UX-P1+P2 batch (same directory).

## Landings covered (chronological, all on main)

1. **REALTIME_3D P5 viewport** (`34a38a45`) — P5a navigation math + overlays, P5b persistent `ViewportSession` + input classification, P5c panel wiring (TexturePane path, `v` toggle in the graph editor). D9 byte-proof green at every step: show output byte-identical with the viewport open. Level: L2 (PNGs reviewed; the flow driver cannot reach the editor window — VD-030).
2. **UX-P3a** (`ee30d52d`) — mod buttons on scene rows via exposure-on-demand (`ToggleNodeParamExposeCommand`, named `<Owner> · <Label>`), save→reload modulation proof in manifold-playback. Level: L3 for exposure flow; value-modulation display headless-unobservable (BUG-234, VD-031).
3. **IMPORT_RESPONSIVENESS P2+P3** (`4eadc39d`) — shared validation device (no per-import `GpuDevice::new`), background import worker + stage toasts + Failed toast; transport proven advancing during a real 43MB import. BUG-219 narrowed: no headless crash in 14 imports; live suspect = UI-thread stall (now removed by P3). Peter's crash-dialog-vs-beachball answer still wanted. Level: L3 (drain-loop) / L2 (worker runtime evidence).
4. **UX-P3b-i** (`76251784`) — mod-button parity on Light/Camera/Lens/Modifier rows + key-stride collision proof + a latent light-row key collision fixed. Level: L3.
5. **REALTIME_3D P6 gizmos** (`b4d2d448`) — object pick (center-distance approximation, documented), move/rotate/scale gizmos → `EditingService` commands with undo/redo round-trip, wired-axis visible refusal, W/E/R modes. Level: L2 (VD-030 applies).
6. **GLB triage + pivot** (`27247490`, reported in its merge) — BUG-221 FIXED (per-object recenter; rotation spins in place; 148-asset conformance green); BUG-219/220/222 diagnosed with probe evidence; BUG-226 logged.

## Superseded mid-wave (recorded so the history reads honestly)

SCENE_PANEL_UX's bespoke row rendering (D2/D3) shipped and was then superseded the same day by `docs/SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md` after Peter's review ("use the same widgets etc from the cards… bugged like crazy"). The convergence rebuild (C-P1a..d) is in flight on `wave/scene-card-convergence`; C-P1a (shared plumbing + Fog family, one-undo-per-gesture) is built pending landing. The exposure mechanism and selection fix from the superseded phases carry forward unchanged.

## Verification debt opened this wave

- **VD-030** — editor-window surfaces (P5c viewport, P6 gizmos) reach L2 only: the flow driver has no graph-editor-window routing, so no scripted-interaction proof exists; click-scripts below are the interim. Burn-down: extend UI_AUTOMATION to the editor window, or Peter's L4 pass.
- **VD-031** — headless flow harness cannot observe driver/envelope value changes (BUG-234) nor slider-fill visual updates (BUG-235): every modulation/scrub L3 claim is dispatch+value-level, not pixels. Burn-down: the verification-infra lane (BUG-225/226/234/235) queued next wave.
- **VD-032** — UX-P2's mid-scrub hairline has no PNG proof (atomic Drag gesture); pinned by unit test only. Carried from the panel landing report.

## Click-script for Peter (~3 min, current main)

1. Graph editor on a scene layer → press `v` over the render_scene node. Expect: viewport with grid/frustum/light overlays; drag orbits, shift-drag pans, scroll dollies; the show output unaffected.
2. Click the cube → axis triad appears. W/E/R cycles move/rotate/scale. Drag an axis: the object moves and ONE Cmd+Z takes the whole drag back.
3. Wire-drive a transform axis in the graph, reselect: that axis renders gray and refuses the drag.
4. Drop `ABeautifulGame.glb` mid-playback. Expect: timeline keeps playing; "Importing — parsing…" toast; layer lands and plays. Drop a corrupt file: error toast, no silence.
5. Scene Setup → any object → click a row's ∿ button. Expect: `<Object> · <Param>` appears on the generator card; assign an LFO there; the object responds live.
6. Known-bugged until the convergence batch lands: fog/env rows are already card-style on the branch; object/light/camera rows are still the old dialect on main (Peter's screenshots) — C-P1b..d replace them.

## Slot/branch state at report time

`wave/scene-card-convergence` live (C-P1b building in slot-3). All other lanes landed and released; ancestor checks green on every retired branch.
