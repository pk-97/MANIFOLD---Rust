# Verification Debt — the unverified-surface ledger

<!-- index: Live ledger of shipped-but-not-fully-verified surfaces. One entry per gap between the verification level a landing reached and its target; burned down or consciously carried every wave. Format and rules: DESIGN_DOC_STANDARD.md §10. -->

Why this exists: "unverified interactively" notes used to live in landing reports and
memory, where they decayed silently into "shipped" — the bugs Peter found in the app
on 2026-07-05 (automation lanes present but not visibly working, glb import behaving
hard-coded) were all previously *recorded* as unverified, and nothing acted on the
record. This file makes the debt durable, in-repo, and impossible to close by
forgetting.

Rules (normative home: `DESIGN_DOC_STANDARD.md` §10):

- Every landing appends one entry per gap between the level reached and the target
  level (L0–L4 ladder in §10).
- Every orchestration wave either burns entries down (verify → move to **Closed**
  with date and how) or consciously carries them — the landing report says so.
  Silence is not carrying.
- IDs are stable (`VD-NNN`), never renumbered — they are referenced from landing
  reports (committed under `docs/landings/`, per §8.10), BUG_BACKLOG `Escaped:`
  lines, and memory.

---

## Open

### VD-030 — editor-window surfaces (REALTIME_3D P5c viewport, P6 gizmos) reach L2 only
Landed 2026-07-17 (`34a38a45`, `b4d2d448`; `docs/landings/2026-07-17-scene-3d-ux-wave.md`). The flow driver has no graph-editor-window routing, so navigation and gizmo drags are proven by production-function tests + orchestrator-reviewed PNGs, never a scripted real-input pass. Burn-down: extend UI_AUTOMATION to the editor window (flow-driver routing for the second window), or Peter's L4 pass on the click-script.

### VD-031 — modulation/scrub L3 claims are dispatch+value-level, not pixels (harness gaps BUG-234 [FIXED] / BUG-298)
Landed 2026-07-17 (UX-P3a `ee30d52d`, convergence C-P1a on-branch). The `--script` harness never runs a content-thread tick (BUG-234) and doesn't visually update slider fill mid-flow (BUG-235), so every "value modulates / scrub moves the slider" claim rests on Rust value tests plus static PNGs.
**BUG-234 half burned down 2026-07-21** (lane/bug234-modulation-tick): `Step`'s frame loop now runs `evaluate_modulation` (drivers + envelopes) plus `reconcile_state` every stepped frame, so a flow CAN assert a changing param value across `Snapshot`s. `scripts/ui-flows/envelope-modulation.json` (new `envmod` fixture) proves it: a Bloom `amount` envelope reads base 0.50, then one frame after the clip's rising edge the Amount row NO LONGER shows 0.50 (asserted as `Count 0` on the base value — a machine-independent "the value moved off base", not a pinned decayed float; the Dump captures the actual pulled-toward-target value, ~4.85, as evidence), then returns to exactly 0.50 once elapsed clears the 1-beat decay — confirmed to fail without the wire. `reconcile_state`'s `sync_param_value` drives the slider's fill/thumb norm off the same `effective` value as the text, so the visual-fill half is very likely fixed too, riding the same wire. **Stale cross-reference resolved 2026-07-21:** the "BUG-235" this entry originally pointed at (slider-fill-mid-flow) had been renumbered away (current BUG-235 is `manifold-own-kick-fixtures-systematic-adtof-timing-bias`, unrelated audio timing); the slider-fill-under-modulation gap is now tracked as **BUG-298** (LOW, pixel-verification only). Burn-down remaining: render the modulated frame and PNG-diff the slider fill region (BUG-298), then close this entry.

### VD-033 — C-P1c's sun-intensity render proof is subtle to the eye; the pixel-diff assertion carried the gate, not a look-pass
Landed 2026-07-18 (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d landing session, reviewing C-P1c's own carried artifacts). BUG-237's render-level closure (`bug237_light_camera_commit_render_proof.rs`) rendered SceneStarter before/after `sun.intensity` 1.0→8.0 and measured `mean_abs_diff 0.09` — real, but visually subtle in the two PNGs (`/tmp/bug237_sun_intensity_{before,after}.png`) an orchestrator eyeballing them side by side would likely call "brighter, maybe" rather than an obvious win; the camera-orbit half of the same proof (`mean_abs_diff 0.06`, a quarter-turn reframe) reads unambiguously by contrast. The gate is real (the pixel-diff assertion is exact, deterministic, and correctly wired to the actual committed value), so this is not a correctness gap — it's a legibility note for whoever next does a look-pass over the Scene Setup panel's Light family: don't rely on eyeballing a static before/after pair for intensity changes this small; trust the numeric diff, or re-render at a starker delta (e.g. 1.0→20.0) if a demo PNG needs to read as obviously-brighter to a human. Burn-down: no action required unless a future look-pass wants a punchier demo asset.

### VD-028 — VOLUMETRIC_LIGHT_DESIGN P1–P3: mechanically L2 (PNGs rendered and read by the orchestrator), Peter's look-pass not yet run, and the demos read as a visual miss
Landed 2026-07-13 (`docs/landings/2026-07-13-volumetric-light-p1-p3.md`). All numeric gates
(V1–V6, CPU-vs-GPU parity, monotonic performer faders, content-thread perf) pass across both
phases with a real acceptance-demo PNG at each look-critical phase (P2 Sun-only, P3 night-garden
multi-light) — reaching L2 by the letter of §10. But unlike CINEMATIC_POST's VD-020 (numerically
green, look unconfirmed but not known-bad), this entry's L2 evidence is itself a negative
result: the orchestrator looked at all four demo PNGs and neither one reads as "a black void
filled with haze with beams of light shining through" — P2 shows an ordinary dim lit scene with
a faint shadow patch over a light-gray (not black) background; P3's night-garden shot shows a
checkerboard void, two dim pillar silhouettes, and a soft ambient glow blob, with no legible
directional beam in either. Burn-down: this is not a "run the demo" gap, it's a "the shipped
math doesn't produce the intended look" gap — Peter's look-pass decides whether this needs a
D2/D3 amendment (the landing report's recommendation) or is accepted as a first pass to iterate
on later. Do not close this entry by re-running the existing demo; closing requires either a
design-level fix that visibly changes the PNGs, or Peter accepting the current look as a
baseline to build on.

### VD-027 — mechbugs wave: BUG-123/038/079 fixes reached L1 (tests green) only, not L2 (observed)
Landed 2026-07-13 (`docs/landings/2026-07-13-mechbugs-wave.md`). Three fixes in the wave have no
render/log/toast actually produced and read by a person: BUG-123's `mesh_edges` `active_count`
guard (a visual-artifact fix — the absence of the bright dot has not been confirmed on a real
scene render), BUG-038's Ableton log throttle (the once-then-debug pattern has not been observed
in a real console run with Live absent), and BUG-079's missing-preset toast (the toast has not
been seen firing on a project with a genuinely unresolvable preset ref). All three are unit-tested
at the value level and reachable headless or via a short manual run — none needs the live rig.
**Burn down:** the wave's own click-script (in the landing report) is exactly this — three short
manual checks, ≤2 minutes total.

### VD-024 — AUDIO_SETUP_DOCK P3b: AudioTriggerSection has no unit-test module
Landed 2026-07-10 (`5c4fbcca`; `docs/landings/2026-07-10-audio-dock-p3b.md`). The new
`crates/manifold-ui/src/panels/audio_trigger_section.rs` lacks the `#[cfg(test)]` collapse/click
module that its siblings `macros_panel.rs` and `layer_chrome.rs` carry. Covered for now by
compile + the 658-test `manifold-ui` suite (no regressions) + the L3 add-trigger flow
(`scripts/ui-flows/audio-clip-trigger-add.json`, 14/14 `ok`). **Burn down:** add a test module
mirroring `macros_panel.rs`'s (default-collapsed, toggle, add/remove row, row-expand) — fold into
P3c or P4, both of which touch adjacent code.

### VD-023 — LIVE_RECORDING_PROOFS P3: in-app record-button glue — L4-by-live-use only, no automated test
Deferred 2026-07-10 (Peter's call; `docs/landings/2026-07-10-live-recording-proofs.md`). P1+P2
prove the recorder itself (the `LiveRecordingSession` API into the real AVAssetWriter, adversarial
timing, independent file verification). The remaining **unexercised glue** — the record
button emitting `ContentCommand::StartLiveRecording`, and the capture block inside the live
compositor frame (`content_pipeline.rs:2547`) — has **no automated test**: P3's intended vehicle
(`ui-snap`) turned out to render the UI tree with no live compositor, so it can't drive this path
(see the design's P3 note). Today this glue is verified only at **L4 by Peter pressing record
live at every show**. Residual risk: a future code change that unhooks the button-to-recorder
wiring would not be caught by a test — only at the next soundcheck/show. Close by building the
headless integration harness described in the design's §8 Deferred P3 entry (a real content
thread + compositor smoke), or accept L4-by-use as sufficient and mark this consciously carried.

### VD-022 — LIVE_RECORDING_PROOFS P2: full-scale pre-gig soak + BUG-086 — L2 reached / L4 carried
Landed 2026-07-10 (`docs/landings/2026-07-10-live-recording-proofs.md`, P2 @ `091290e3`). The
`recording-soak` bin and its decoded-index PASS gate are verified at L2 via a short 1080p/2-minute
run the orchestrator executed and whose `.mov` it opened. **Two carried gaps:**
(a) The **full-scale 4K60 20-minute soak has never been run** — by design (§6 P2), its first
execution is Peter's pre-gig ritual on the rig; the short soak is the wave's proxy, so the show
configuration at full data volume (~17.5 GB, past every historical failure threshold) is L2/proxy,
not L4/real. Close when Peter runs the full soak and it PASSes on the rig.
(b) **BUG-086 CLOSED 2026-07-13** (recording-sync lane) — root cause found and fixed: the
shortfall was `recording_soak.rs`'s own synthetic-audio ring-buffer pusher silently discarding
overflow under unpaced/encoder-stress timing bursts, not the native encoder (whose backpressure
gate measured 0 drops throughout, both before and after this fix, and was hardened separately
per the class rule). Verified: 3x paced 2-min 1080p soaks at <0.01% off intended duration, and
the unpaced repro that previously fell short by 1.2-3.3% now measures full coverage across 3
reruns. See `docs/BUG_BACKLOG.md` BUG-086 for the full diagnosis.

### VD-021 — PROJECT_FILE_INTEGRITY P1: save durability under power loss — L1 reached / L1 carried
Landed 2026-07-09 (`docs/landings/2026-07-09-project-file-integrity.md`). `save_v2_archive` now
`sync_all()`s the temp file's *contents* before the atomic rename (BUG-064), keeping the existing
parent-directory fsync. Verified at L1: code inspection + a negative gate asserting two `sync_all`
calls (temp-file + parent-dir), and the full save/load + history round-trip suite stays green. The
actual property — a mid-save power cut can no longer replace a good archive with a torn one — is
not unit-testable without fault injection (interpose on fsync/rename, kill between them, assert the
old file survives). Consciously carried: the fix follows the documented write→fsync→rename ordering;
deeper proof needs a crash-consistency rig no other MANIFOLD test has. Burn-down if ever warranted:
an `LD_PRELOAD`/`dm-flakey` crash-consistency harness, else Peter accepting L1 for a one-line
ordering fix.

### VD-020 — PARAM_STORAGE_BOUNDARIES P2: calibration-drag gesture is L1, not L3 — L1 reached / L3 target
Landed 2026-07-09 @ P2 (`254792c0`). The card-rendering half reached L3
(`scripts/ui-flows/calibrated-param-card-reads-manifest.json` — inspector renders
Mirror/Bloom/Strobe cards with manifest-sourced ranges, PNG confirmed). The literal
"drag a calibrated slider → reload → real degree range" gesture is only L1 (the
manifold-core regression `calibrated_param_derives_meta_params_on_save_not_the_stale_shadow`),
because it lives in the graph-editor mapping popover and `ui-snap --script`'s scene
whitelist excludes the graph editor (dump-only). **Burn-down:** extend the UI_AUTOMATION
`--script` harness with a graph-editor scene, then script the drag+reload. Until then the
round-trip is Rust-proven, not interaction-proven. L4 (Peter dragging it live) remains the
ultimate target.

### VD-001 — Automation lanes P1–P4: runtime pointer→command editing path — **L3 reached** / L4 target
Landed 2026-07-04 @ `8b306de0`. **L3 burned down 2026-07-05 (Opus):**
`scripts/ui-flows/drag-automation-point.json` resolves Mirror's middle breakpoint by its
`automation_lanes` surface target and drags it through the real input path (pointer →
viewport events → `InteractionOverlay` → `AppEditingHost` → automation command on the scene
Project); the re-dump confirms the point moved 607.88 → 622.55px, x/beat and the other points
unchanged. The runtime pointer→command→redraw automation-editing core **works headless**.
**Consequence for the in-app bug:** since the core edit path is functional, the "lanes present
but not visibly working" Peter hit is downstream of this core — live-app rendering/visibility or
a live-only wiring the headless overlay path bypasses. **L4 (Peter watching a lane edit take
effect in the running app) remains the target** and is where the real symptom must be triaged.
**07-07 update (timeline-ux audit):** the LANES transport toggle is also proven headless
end-to-end through the REAL `ui_bridge::dispatch` (`scripts/ui-flows/toggle-lanes.json` —
strips off/on, asserts + PNGs), and the in-app symptom is root-caused as an EXPOSURE gap, not
a wiring break: no UI path creates a first lane (AUTOMATION_LANES §7 chooser unbuilt), so
LANES on a lane-less project visibly does nothing. See `docs/TIMELINE_UX_AUDIT_2026-07-07.md`
§1. Peter's L4 residue narrows to: confirm LANES lights live + ARM-record a first lane.
**2026-07-16 (Peter):** automation lanes still need substantial UI/UX work (the §7
first-lane chooser among it) before the L4 confirm is meaningful — stays open, blocked
on that UI/UX pass, not on a rig session.

### VD-002 — Preset library + picker P0–P6: interactive GUI matrix — L2 reached / L3 target — **driver-reach blocker REMOVED 2026-07-07**
**07-07 update:** option (c) below shipped on `fix/timeline-ux-pass` — the `--script` driver
now routes every `PanelAction` through the real `ui_bridge::dispatch` (`UserPrefs::in_memory()`
preserves D7 determinism), so open-picker dispatch is headless-reachable. Remaining burn-down:
author the four picker flows as `--script` JSONs; search-text steps stay excluded until
`AutomationAction::Text` gets a headless seam.
Landed 2026-07-04/05 (last `4c860cad`). Drag-drop, search-clear, the management
matrix, and thumbnail display are physically unautomatable headless today.
**Correction 2026-07-05 (Opus): NOT runnable at L3 via the current P2 `--script` driver.**
The earlier "now runnable" note assumed the driver could open and drive the picker; it can't.
Opening the Add-Effect browser / preset picker routes through app-level `PanelAction`s that the
driver's `apply_panel_actions` deliberately does **not** implement (it handles only
`LayerClicked`; the rest need `ui_bridge::dispatch` + `UserPrefs::load()`, breaking headless
determinism). The picker panel lives in `UIRoot` but nothing in the two proving scripts opens
it, and there is no `AutomationAction` to open a popup. So the four picker flows need one of:
(a) a P3 live door into a running app, (b) new scene fixtures that pre-open the picker (single
frame only — no animation, no search text since `AutomationAction::Text` has no headless seam),
or (c) a driver extension that implements the open-picker dispatch headlessly. Until then the
interim Peter click-script (L4) is the only path. Same reach gap blocks the BUG-026 frame-0
repro (see VD-006).

### VD-003 — glTF import: correctness beyond the development fixture — **L2 reached (geometry)** / L2 target; textured + production-drop path still owed
Landed with the glTF wave (foundation @ `47c878d7` + follow-ups). Peter reports
in-app import behavior "hard-coded or buggy" (2026-07-05, exact repro not yet triaged).
**Held-out geometry gate DONE 2026-07-05 (Opus):** both held-out fixtures rendered through
the `render_scene` mesh-snapshot harness (input now overridable via `MESH_SNAP_GLB`, commit
`e7560cb4`) and were **looked at**, not just pixel-counted:
- `lowe.glb` (1.9M verts) → a clean, correctly-formed lion statue on its plinth.
- `cc0__japanese_apricot_prunus_mume.glb` (4.3M verts) → faithful blossom branches, PLUS a
  stray small cube beside the model. Triaged: the cube is a leftover Blender default-cube
  (`Material.001`) baked into that source asset — lowe (a different pipeline) has none — so it
  is a data problem in the asset, **not** an import bug.
Conclusion: glTF **geometry** import is faithful and not hard-coded; the "hard-coded or buggy"
symptom is NOT in the geometry path. **Still owed** (does not gate L2, but keeps this open as a
lens on Peter's report): (a) the *textured* path (this harness applies a default green material,
no albedo — see the separate `gltf_textured_azalea…` proof for the textured route), and (b) the
*production drop-import* UI path (dragging a `.glb` into a layer), which is what Peter actually
exercised. **Fixtures are large** (lowe 43 MB, apricot 85 MB) and remain untracked — committing
127 MB of binaries is Peter's call (recommend git-lfs or keep them as local held-out assets).

### VD-006 — BUG-026 batch-2 popup entrance-tween fix: running-app confirmation — L2 reached / L4 target
Fix landed 2026-07-05 (commit `01c15213`) for the "no popup background until mouseover" bug —
root-caused as a missing animation-poll (see BUG-026). Gate green (clippy; `manifold-ui --lib`
604/604), but the headless `--script` driver has no frame loop and ticks `enter_anim` off
wall-clock, so it **cannot** exercise this timing bug. Burn-down: open the Add Effect browser in
the running app and confirm the dark background panel is present immediately, before moving the
mouse (and that the fade-in reads smoothly). Peter owns this L4 observation.

### VD-011 — AUDIO_SENDS_UX P1 per-send gating: trace-count run with real audio — L1 reached / L2 target
Landed 2026-07-06 (`docs/landings/2026-07-06-audio-sends-ux.md`). The consumed-set walk is
unit-proven (4 tests on `Project::analysis_consumed_sends`) and the per-send skip is in the tick
path, but the doc's own P1 gate — `MANIFOLD_AUDIO_TRACE=1`, 16 sends, one bound param, log shows
"analyzed 1 send(s)" (2 with the scope open) — needs a running app with a capture device. The
instrument is shipped and env-gated. Burn-down: Peter (or a future L3 flow) runs the trace launch
per the landing report's click-script step 5. Peter owns the L2 observation.

### VD-013 — ABLETON_TRANSPORT_SYNC: closed-loop transport against real Ableton — L1 reached / L4 target
Landed 2026-07-07 (`docs/landings/2026-07-07-ableton-transport-sync.md`). The state machine is
proven at L1 (16 transition tests + 8 failure-catalog scenarios incl. play-from-cursor drag-back,
packet loss, 400ms scheduler), but every scenario runs against FakeAbleton — real Live's listener
cadence, `set current_song_time` during playback, and SPP-on-relocate behavior are modeled, not
observed. Burn-down: Peter's 7-step live checklist (design doc §6 P4 demo) — play-from-cursor both
sides, scrub during playback, rapid play/stop drumming, tempo ramp, IAC-kill degrade test, loaded
machine. The checklist IS the acceptance gate; the design's safety property (unconfirmed
expectation never moves the playhead) bounds the worst case at no-worse-than-before.

### VD-014 — KICK_SWEEP_EVENT P2: content-thread cost + live kick feel — L1 reached / L4 target
Landed 2026-07-07 (`docs/landings/2026-07-07-kick-sweep-p2.md`). The ridge detector is proven at
L1 (exact-match fire counts on all 10 fixtures + green guards), but two things are unmeasured.
(1) The `MANIFOLD_RENDER_TRACE` content-thread gate was reasoned, not run: the per-hop work is a
bounded, allocation-free peak-pick + ≤12 short track updates, dwarfed by the CQT, but no live
trace confirms no frame >20ms with an audio-layer send bound. (2) The kick feel is Peter's P3:
does it catch kicks on a bass-heavy finished track, does it strobe on bass, is the ~50–65ms
confirmation latency (D7) acceptable. Burn-down: the landing report's ≤2-min click-script;
`KICK_WIN` is the latency knob if it reads late.

### VD-015 — BUG-052 sample-rate invariance: end-to-end cross-rate fire-time match — L2 reached / L3 target
Fixed 2026-07-07 (`6e0e8988`). Grid invariance is L2-proven by unit test (hop/window duration hold
across 44.1/48/88.2/96/192k) + the green analysis suite, so all hop-count constants keep their
wall-clock meaning by construction. What's unproven is the original gate: take one fixture, resample
it 48k→96k, run `mod_harness` at both rates, confirm the kick/onset fire TIMES in seconds agree (the
eval grades in seconds). Cheap and deterministic — a follow-up harness run, no rig. Closes at L3 on
that match.

### VD-016 — PARAM_STEP_ACTIONS P2: content-thread cost of per-layer clip-edge tracking — L1 reached / L4 target
Landed 2026-07-08. The clip-edge tests are proven at L1 (timeline start / session-slot launch /
Transient-vs-Clip-vs-Both mode gating, all green), but the `MANIFOLD_RENDER_TRACE` content-thread
gate was reasoned, not run — same wall VD-014 hit: the trace instrumentation lives inside
`content_pipeline.rs`'s live render path, gated on a real `GpuDevice` and only reachable through the
running app's event loop; the `ui_snapshot` harness (`cargo xtask ui-snap`) renders UI/graph PNGs off
a bare `GpuDevice` but never drives `ContentThread`/`content_pipeline.rs`, so there is no headless
path today. Reasoned bound: the addition is one `AHashMap` insert + one small `Vec` push per actual
clip START (not per-frame — rare relative to tick rate) plus a bounded scan over that same
per-layer-count-sized `Vec` during modulation, the same allocation-free scratch-reuse shape as the
already-shipped `pending_trigger_pulses`. Burn-down: `MANIFOLD_RENDER_TRACE=1` live against the
53-layer Liveschool fixture with a Clip-mode step mod armed, confirming no frame >20ms.

### VD-017 — PARAM_STEP_ACTIONS P3: performer gesture untried live — L3 reached / L4 target
Landed 2026-07-08. The drawer's Action/Amount/Wrap rows are proven at L3 (a `scripts/ui-flows/`
script drives the real click path, sets Action=Step, asserts the badge; a separate integration
test proves save→reload→fire resumes from the committed base) — but nobody has felt the actual
performer gesture live: point the Kick send at BasicShapes' `variant` param, arm Step/Wrap, play a
4-bar loop, watch the shape advance per kick and wrap cleanly at the rail. That's Peter's L4,
owed. Burn-down: the click-script below in `docs/landings/2026-07-08-param-step-actions.md`.

### VD-018 — UI_CLIP_AND_Z P1: D2 tier-stacking not enforced on the live main-window path — L1 reached / L2 target
**Correction 2026-07-08:** the claim below that "containment kills BUG-060 (proven at L2)" was WRONG
— `bug060.after.png` renders through `traverse()`, not the live `panel_cache_info`/UICacheManager
path, so it proved containment in a render path the app never uses. BUG-060 still repros live and is
REOPENED (see BUG_BACKLOG). This VD is now bigger than a stacking nicety: the live cache path getting
the region treatment (clip *and* order) is what an actual fix likely needs, so treat it as coupled to
BUG-060, not deferred cosmetics.

Landed 2026-07-08. Containment (D1) is enforced everywhere — the inspector region clips at its
rect, which is what actually kills BUG-060 (proven at L2, `bug060.after.png`). But *declared
stacking* (D2) is tier-sorted only on the `traverse()` render path (headless snapshots + the editor
window); the live main-window path renders via `panel_cache_info()` + `UICacheManager`, which is
array-ordered, so D2's "Chrome always wins over Base regardless of build order" is carried on that
path by containment alone, not by tier order. Invisible today because no main-window Base/Chrome
regions overlap (disjoint `ScreenLayout` rects) — it becomes load-bearing the first time two tiers'
regions overlap on the main window. Burn-down: P2 unifies the cache path onto tier-ordered region
traversal (or makes `panel_cache_info` emit in `(tier, insertion)` order), then a PNG with a
deliberately overlapping Base/Chrome pair proves the Chrome wins. Flagged to Peter at landing as the
one design-level call; accepted for P1 because BUG-060 dies by containment independent of it.

### VD-019 — UI_CLIP_AND_Z P1: BUG-060 L3 flow scrolls the inspector but not to true bottom — L2/L3 partial / L3 target
Landed 2026-07-08. The acceptance flow (`scripts/ui-flows/bug060-inspector-footer-containment.json`)
drives the real click path and proves the footer stays hit-testable and dispatches with the inspector
busy and scrolled — but `try_inspector_scroll`'s effective max is ~15-20px on content ~1200px too
tall (BUG-076), so it never reaches the *very bottom* that is BUG-060's exact repro condition.
Containment makes bottom-scroll safe by construction (the region clip is unconditional), so this is a
demonstration gap, not a correctness one. Burn-down: fix BUG-076 (scroll estimator under-counts
drawer-open card height), then re-run the flow to a true bottom and re-capture.

*(VD-001–004 seeded 2026-07-05 from the memory corpus plus Peter's in-app findings; VD-006 added
2026-07-05, VD-007 at P2 landing, VD-008 at P3 landing, VD-009 at P4 landing, VD-010 at P5-inspector
landing. VD-005 closed at P2 landing. The full backfill pass over recent landings is still owed and
will extend this list.)*

## Closed

### VD-035 — scene-setup-modifier-stack flow unrunnable headless (BUG-293); modifier add/reorder pinned at value level only — CLOSED 2026-07-21 (WS3 lane/ws3-queryable-rows: queryable row names + ScrollTo; flow green unmodified-intent)
**CLOSED 2026-07-21 (WS3 `lane/ws3-queryable-rows`).** The §5b fix shape above landed: `build_param_row`/`build_toggle_trigger_row` now emit param-id-derived names on every converged card row (`param_row.<id>[.slider|.value|.driver_btn]`), and a new `AutomationAction::ScrollTo` scrolls a deep row into view before a Pointer step acts. `scripts/ui-flows/scene-setup-modifier-stack.json` is re-authored at ORIGINAL fidelity — add + value + slider **DRAG** + driver btn + reorder + undo — and runs **green, unmodified-intent** on the real gltfscene GPU path (30/30 steps): `ScrollTo` reaches the deep inspector `param_row.15_angle.slider` (y≈7789 → on-screen), the drag executes on the named track, `param_row.15_angle.driver_btn` asserts by name, the two `↑` reorder buttons resolve, and the four Cmd+Z presses report `undo_count` 2→1→0. The close condition (green unmodified-intent, drag/driver asserts intact) is met — no weakening. Same run also closes BUG-239 (the value cell tracks the dispatched write).
Found 2026-07-21 during the scene-convergence P5 flow sweep (14/15 scene-setup flows green; this is the 1 failure). The flow's modifier add goes through a context-menu action, and the `--script` driver drops `host.pending_actions` on the floor (BUG-293), so "Twist" never appears and the flow fails at its first post-add assert — a harness gap, not an app regression (the flow was green 2026-07-17 under the pre-convergence add path; the live app is unaffected). Burn-down: fix BUG-293 (route `pending_actions` like `app_render.rs` does), then re-run this flow unmodified; it is deliberately NOT re-pointed or weakened in the meantime.
**2026-07-21 update (verif-infra-flow-driver lane):** BUG-293 fixed (`pending_actions` now routed through `apply_panel_actions`, `script.rs:805-817`), verified NOT the whole story: the flow **still fails**, identically, before AND after the BUG-293 fix — confirmed by running both the pre-fix and post-fix binary against this exact flow. The failure is NOT a pending_actions drop: `+ Add Modifier`'s click resolves and reports `ok`, but at a resolved screen position of `(736.0, 3430.0)` — far below `LOGICAL_H` (1216.0) — so whatever the click actually hits (if anything) never produces a dropdown with "Twist" in it, `07.fail.tree.json` shows no dropdown content at all. This looks like separate fixture/layout drift (the Scene Setup dock's `+ Add Modifier` button now renders far below the fold in this fixture, likely BUG-294-adjacent but not the same fix) rather than the pending_actions gap this entry names. Left OPEN — do not close on the BUG-293 fix alone; the real blocker for this specific flow needs its own investigation.
**2026-07-21 RE-DIAGNOSIS (scene-convergence P3 landing, orchestrator) — NOT a layout bug.** Instrumented `ScenePanel::handle_scroll` under the gltfscene fixture: the content DOES overflow correctly (`content_height ≈ 3384`, `viewport ≈ 1098`, `max_scroll = 2286`, `overflows=true`) — the panel knows the "+ Add Modifier" row is below the fold and the scroll range covers it. The blocker was the **scroll delta SIGN**: `ScrollState::apply_scroll_delta` does `scroll_offset -= delta * SCROLL_SPEED`, so a POSITIVE `y` delta scrolls UP and clamps to 0. The flow (and the `scene-setup-add-fog-drag` flow it was modeled on) used positive `y: 900`, so `offset` stayed pinned at 0 and the button never moved — which reads as "unreachable." A NEGATIVE delta scrolls down as intended: proved `offset 0→900→1800→2286` (clamped at max), landing the button on-screen (`3384 − 2286 ≈ 1098`, within the viewport). **The live app is unaffected** (a real mouse wheel feeds the correct sign); this is purely a `--script` flow-authoring error. With the scroll fixed, the flow then fails on **stale pre-convergence assertion names** (`scene_setup.modifier.param1_value` / `param1_track` / `param1_driver_btn`) — those bespoke ids are gone (the panel names only `scene_setup.properties.*` now); the modifier param rows render as ordinary converged card rows (the value IS present in the tree — no regression, NOT a P3 deletion casualty). **Fix = flow-suite re-authoring** (add the negative-delta scroll + re-target the modifier drag/driver/reorder asserts at the converged card rows) — design §6 P4's "rewrite the scene flow suite against the real rows," deliberately deferred beyond the P4 docs-only supersession sweep. VD-035 stays OPEN but is now FULLY diagnosed; no code bug, no new BUG-NNN.
**2026-07-21 INSTRUMENTED CONFIRMATION + partial-re-author blocker (WS2 bug-debt wave, Opus orchestrator).** Ran a probe flow (`cargo xtask ui-snap gltfscene`) to reproduce the two claims above before re-authoring. Confirmed: (1) the negative-scroll prescription is correct — a `Scroll { delta.y: -2400 }` over "+ Add Modifier" moves it from y≈3430 to y≈1144 (on-screen), the dropdown opens, and Twist is added (all steps green). NOTE the working `scene-setup-add-fog-drag` flow uses a POSITIVE `y:300` and is green only because its target sits at a shallower depth — do not model the modifier scroll on it; the modifier section needs the large negative delta. (2) The partial-re-author blocker is worse than "stale names": the converged Twist-modifier param rows (Axis/Angle/Center) carry **only text labels, `name=None`** — the entire Scene Setup panel emits exactly ONE bespoke widget name (`scene_setup.properties.name_value`) and the card protocol names only `inspector.param_card.mapping_chevron`. So the value/reorder(`↑`)/undo asserts ARE re-targetable (by `text`), but the original `param1_track` (slider **drag** target) and `param1_driver_btn` have NO queryable selector, and the modifier param rows render very deep (Angle at y≈7789, past even the max scroll). Closing VD-035 at the ORIGINAL fidelity therefore needs a harness affordance, NOT a pure flow-authoring change. **RULING (Peter/lead 2026-07-21): (c) carry — approved; (b) reduced-fidelity is REJECTED** (the close condition is green UNMODIFIED-intent; dropping the drag/driver asserts is exactly the weakening this entry forbids). **Fix shape is NOT bespoke — it is the `docs/WIDGET_TREE_DESIGN.md` §5b row-affordance recipe** (emit queryable names on the converged rows, machine-enforced by `no_bespoke_row_infra`), plus a deep-scroll-to-target harness helper. It **rides the in-progress WIDGET_TREE execution** (whose P5 status already lists this flow as the 14/15 gap) — do NOT author it ad-hoc as a standalone task. Consciously carried until then (verification debt; live app unaffected — a real wheel/drag works). Probe artifacts: `scratchpad/vd035-probe*.log`, `target/ui-snapshots/gltfscene/run-vd035-probe/10.tree.json`.

### VD-034 — WIDGET_TREE P3: card-drag L3 flow variant not authored; geometry proven at L1 math + generic drag flow only — CLOSED 2026-07-21 (BUG-296 fixed, `inspector-card-drag-reorder.json` authored)
Landed 2026-07-20 (P3 lane `cb2347b6` + P1a, merged together). The P3 gate names "drag flow (the `drag-clip.json` pattern, card variant)"; no card-drag flow existed on disk, so the landing ran the existing `timeline` `drag-clip.json` (green, L3, proves the drag input path generally) plus the new pure-math drop-target family and the INV-3 pin (`inv3_drag_targets_follow_live_bounds_after_in_place_scroll`, which drives through to the dispatched `EffectReorder`). Burn-down note (P5 sweep) said authoring was blocked on BUG-296 — the drag fires the real input path (`dispatched EffectReorder(0, 2) (structural=true)` in the `inspector` fixture scene) but the `--script` driver never rebuilds the inspector cards after a structural dispatch. **Closed 2026-07-21 (verif-infra-flow-driver lane):** BUG-296's real root cause turned out to be different from the suspected one — `Runner.active_layer` was never seeded from the fixture's actual active layer, so a structural dispatch silently mutated the wrong layer's (empty) effect chain instead of the displayed one; not a missing rebuild. Fixed by seeding `runner.active_layer` from `data.active` in `script.rs::run` (~line 184). `scripts/ui-flows/inspector-card-drag-reorder.json` now proves the card-drag reorder end-to-end under the `inspector` scene: drags Mirror's KEY_DRAG handle onto Strobe, dispatches the real `EffectReorder(0, 2)`, and asserts the post-drag label rects — `[Mirror,Bloom,Strobe]` → `[Bloom,Mirror,Strobe]` (index 2 lands the dragged card at index 1, not 2 — the real result, not the originally-assumed index). `cargo xtask ui-snap inspector --script scripts/ui-flows/inspector-card-drag-reorder.json` exits 0, 9/9 steps.

### VD-032 — UX-P2 mid-scrub hairline unproven in pixels — CLOSED 2026-07-18 (MOOT, SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d)
Landed 2026-07-17 (`6dd700e7`) as an open gap: the ui-snap Drag gesture is atomic (no mid-drag snapshot), so the old bespoke scrub-hairline draw was pinned by unit test only. C-P1d (2026-07-18) deleted the entire bespoke row/drag layer this entry was about — `build_modifier_numeric_row`/`build_modifier_enum_row` (Modifier, the last holdout family) and the panel's own delta-drag machinery (`struct ValueDrag`, `object_value_cells`/`object_steppers`, the scrub-hairline draw itself) are gone; every scene param row now scrubs through the card protocol's `SliderDragState`/`BitmapSlider`, whose own visual correctness is covered by `param_card.rs`'s existing golden/unit test suite (not this file's ledger). Nothing left to burn down — the surface the gap was about no longer exists.

### VD-029 — SCENE_SETUP_PANEL_DESIGN P1's fog-drag L3 flow regressed to unreachable by BUG-199 — CLOSED 2026-07-17 (BUG-199 fixed, BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 1)
Landed 2026-07-17 (`docs/landings/2026-07-17-scene-setup-panel-p5-wave-close.md`), closed the same
day. P1 originally reached L3 for the Fog add+drag gesture
(`scripts/ui-flows/scene-setup-add-fog-drag.json`, green at P1's own landing). By P5's landing the
same flow, run against the same azalea fixture, failed: clicking "+ Add Fog" resolved to a screen
position past the window's own rendered height (the Objects section grew across P2–P5, pushing
Lights/Environment/Fog/Camera below the fold), and BUG-199 (neither utility dock's
`ScrollContainer` received a real scroll input) meant nothing could bring it back into a clickable
position. Burn-down, as predicted: fixing BUG-199 (`primary_mouse_wheel` now routes wheel events
over either dock through the generic `UIEvent::Scroll` pipeline) restored L3 for this flow with no
change to its gesture/assertion logic — only a `Scroll` step was added before the "+ Add Fog"
click (the button now sits below the fold by construction, same as the real app). Green again,
15/15 steps, `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-fog-drag.json`.

**2026-07-16 burn-down (Peter's direction)** — the following owed live/look passes were
waived wholesale: the surfaces ship as-is and anything found in use is filed as a new bug
in `docs/BUG_BACKLOG.md`, not reopened here.

### VD-026 — AUDIO_SETUP_DOCK P7 tap-follow + band-dim live confirm — CLOSED 2026-07-16 (waived)
The missing L3 interact-verb infrastructure stays tracked via VD-024 (open).

### VD-025 — AUDIO_SETUP_DOCK P3c fire-meter live RENDER_TRACE run — CLOSED 2026-07-16 (waived)
Risk was already assessed negligible (0.19 µs/tick measured vs 20 ms budget); the live trace requirement is dropped.

### VD-016 — OFFLINE_AUDIO_REACTIVE_EXPORT real-track export feel — CLOSED 2026-07-16 (waived)
Duplicate-ID note: the PARAM_STEP_ACTIONS VD-016 remains OPEN — this closure is the OFFLINE_AUDIO_REACTIVE_EXPORT entry only.

### VD-017 — DRAG_CAPTURE P2 audio-panel-over-timeline seam demo — CLOSED 2026-07-16 (waived)
Duplicate-ID note: the PARAM_STEP_ACTIONS VD-017 remains OPEN.

### VD-018 — DRAG_CAPTURE P2 SelfManaged overlay rect fidelity — CLOSED 2026-07-16
Its own burn-down was already "file a bug if a press-through ever surfaces" — that is now the standing rule, so the entry closes. Duplicate-ID note: the UI_CLIP_AND_Z VD-018 remains OPEN.

### VD-019 — DRAG_CAPTURE P3 band-divider immediate-drag feel — CLOSED 2026-07-16 (waived)
Duplicate-ID note: the UI_CLIP_AND_Z VD-019 remains OPEN.

### VD-007 — PARAM_STORAGE P2 storage-swap live confirmation — CLOSED 2026-07-16 (waived)
Subsumed into Peter's one-time V1.4 library re-save pass (his item, tracked in PARAM_STORAGE_DESIGN.md's closed status).

### VD-008 — PARAM_STORAGE P3 transport topology guard live confirmation — CLOSED 2026-07-16 (waived)
Same re-save-pass subsumption as VD-007.

### VD-009 — PARAM_STORAGE P4 Ableton/OSC by-id hardware round-trip — CLOSED 2026-07-16 (waived)
Same re-save-pass subsumption as VD-007; a mapping that fails at the rig is a bug, filed as one.

### VD-010 — PARAM_STORAGE P5 angle-card degree readout — CLOSED 2026-07-16 (waived)
Same re-save-pass subsumption as VD-007.

### VD-012 — AUDIO_SENDS_UX P3 calibration-drag feel pass — CLOSED 2026-07-16 (waived)
This was the panel's stated acceptance gate; Peter, who owns the gate, closed it.

### VD-020 — CINEMATIC_POST P5/P6 (GTAO + AO denoise) look-pass — CLOSED 2026-07-16 (waived)
Also waives the P4 dof-polish verdict; BUG-137/BUG-138's pending-confirmation notes resolved in the same pass (BUG-136's live-repro escalation stays open). Duplicate-ID note: the PARAM_STORAGE_BOUNDARIES VD-020 remains OPEN.

### VD-021 — GLB_CONFORMANCE G-P1+G-P2 AMG-livery + card-curation look-pass — CLOSED 2026-07-16 (waived)
BUG-165 and the TextureTransformTest fixture gap stay tracked in the backlog / design doc. No conflict with the in-flight GLTF_MATERIAL_EXTENSIONS or animation work — this covered the already-landed AMG livery fix and card curation. Duplicate-ID note: the PROJECT_FILE_INTEGRITY VD-021 remains OPEN.

### VD-004 — Audio layer export mixdown — CLOSED 2026-07-07 (L2 reached)
`audio_mixdown.rs` offline mix was unverified on a real export since it shipped. **Closed by
the OFFLINE_AUDIO_REACTIVE_EXPORT landing**: the P1 byte-identity fixture pins the WAV bytes
across the seam refactor, and the `journey-proofs` `audio_reactive_export_moves` proof runs a
REAL export end-to-end and asserts via ffprobe that the muxed audio stream is present in the
output file. (Listening to a real stem-bearing export is subsumed by VD-016's L4 pass.)

### VD-005 — UI_AUTOMATION P1 selector surface: no scripted drive — CLOSED 2026-07-05 (L3 reached)
Opened at P1 landing (`3294eb9d`, L2). **Closed by P2 landing** — the `drag-clip.json`
flow resolves a `timeline_clips` surface target and drives it through the real input
path (clip moved 230→314px), and `select-and-inspect.json` resolves a widget by
name/text and clicks it: the selector surface is now scripted-driven end-to-end (L3).
Residual carried as an organic-growth item, NOT debt: the `editor` scene still surfaces
zero *named* widgets (graph-editor chrome unnamed headless) — name points as flows need
them, per §3 ("coverage grows organically"). Landing report:
`docs/landings/2026-07-05-ui-automation-p2.md`.

- VD-036 · 2026-07-21 · UI_FUNNEL P-P rider: INV-G4 MANIFOLD_RENDER_TRACE frame-cost spot-check not run (headless flows cover the sync calls, not the multi-window per-frame cadence). Burn down: next interactive session or the P-F landing trace gate. Owner: Wave-1 WS1.
- VD-037 · 2026-07-22 · P-I single-active-gesture invariant: cross-window overlap unexercised by tests (field-asserted only). Burn down: interactive two-window drag smoke or scripted two-window flow. Owner: Wave-1 close.
