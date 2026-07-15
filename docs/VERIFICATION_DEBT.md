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

### VD-026 — AUDIO_SETUP_DOCK P7: tap-follow + band dimming has no L3 flow and no live full-pipeline PNG
Landed 2026-07-11 (`docs/landings/2026-07-11-audio-dock-p7.md`). The phase's own gate asked for an
L3 ui-flow (expand a trigger drawer on send B while the panel shows send A ⇒ tap command for B;
collapse ⇒ restored) and a headless PNG of the dimmed scope with a Low-band trigger open — neither
is reachable in this sandbox. The L3 flow needs a `scripts/ui-flows/` interact verb for
arming/expanding an audio-mod drawer, which doesn't exist yet (none of `select`/`collapse`/
`collapse_effect` reach it). The live-dim PNG needs the VQT waterfall itself producing pixels
(`content_num_bins > 0`, real audio), which is the same live-audio-device gap VD-025 already names
for the waterfall's own render path — dimming rides the identical shader pass, so it inherits the
identical gap. **What IS verified:** the selection logic (`open_fire_mode_drawer_send`/`_band`,
`crates/manifold-ui/src/panels/param_card.rs` tests) at the value level, and the shader's actual
darkening math on the real Metal pipeline (`spectrogram::gpu_tests::
dim_range_darkens_outside_the_kept_band_only`, `gpu-proofs`) — a stronger proof of the pixel math
than a PNG would be, just not of the click-to-pixel round trip. **Burn down:** build the harness
interact verb (co-owned with VD-024's sibling gap — AudioTriggerSection has no test module either,
which is what makes hand-rolling a drawer-open fixture there costly right now), and confirm the
live crossing + dim on Peter's rig alongside VD-025's burn-down (same soundcheck session covers
both).

### VD-025 — AUDIO_SETUP_DOCK P3c: fire-meter content-thread work never run through the live MANIFOLD_RENDER_TRACE
Landed 2026-07-10 (`12fbc37d`; `docs/landings/2026-07-10-audio-dock-p3c.md`). The §5/BUG-035
content-thread-work gate (any per-tick content-thread work → live `MANIFOLD_RENDER_TRACE=1`, no
frame >20ms) was **not run live**: it needs the app with an audio device + GPU, absent in the
build sandbox. Substituted an honest isolated measurement of exactly the new capture work (worst
case 128 configs × 2000 iters): **0.19 µs/tick release, 13.14 µs/tick debug** — ~1500× under the
20ms budget, and the work is bounded stack writes on a `Copy` struct (no heap). **Burn down:**
launch `manifold` on a project with both a `LayerClipTrigger` and an armed `is_trigger_gate` param
mod, audio playing, `MANIFOLD_RENDER_TRACE=1`, and confirm no frame regression — an L4 run for
Peter/a machine with a device. Risk assessed negligible; carried, not blocking.

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

### VD-016 — OFFLINE_AUDIO_REACTIVE_EXPORT: real-track export feel — L2 reached / L4 target
Landed 2026-07-07 (`docs/landings/2026-07-07-offline-audio-reactive-export.md`). Audio-bound
params now move in exported video, proven L2 by the `journey-proofs` harness (click-track
luma ratio ~6.9× click-vs-gap, save→reload survives, two runs bit-identical in extracted
frames) — but only on a synthetic click track and one generator param. Unobserved: a real
master through the full band/floor/crossover settings Peter actually performs with, and
whether the offline capture-substitution (capture-fed sends hear the timeline mix) reads
correctly on a real project. Burn-down: Peter exports one real track with his usual bindings
and watches it — the design doc's stated milestone ("Peter exports a real track and sees the
pump"). One deliberate `cargo test -p manifold-app --features journey-proofs` per audio-path
wave keeps the harness honest (needs ffmpeg/ffprobe on PATH).

### VD-017 — DRAG_CAPTURE P2: audio-panel-over-timeline seam demo — L1 reached / L2 target
Landed 2026-07-08 (`docs/landings/2026-07-08-drag-capture-p2.md`). The z-aware seam guard
(`overlay_contains_point` blocking the split-handle/inspector-edge press when the Audio Setup
panel floats over the timeline) is proven only by unit tests, including one that asserts the
panel and split-handle actually overlap at today's constants before checking the guard. No
headless PNG exists because no `scripts/ui-flows/` scene or snapshot fixture opens the Audio
Setup panel positioned over the timeline, and the brief forbade inventing one. Burn-down:
this collapses into the P3 L4 feel pass — Peter grabs a crossover line sitting over the
timeline-split zone and confirms the line moves while the panels don't resize (the P2
performer gesture). If a repeatable artifact is wanted, add an audio-panel-over-timeline
snapshot scene in a later UI-fixtures pass.

### VD-018 — DRAG_CAPTURE P2: SelfManaged overlay rect fidelity in `overlay_contains_point` — L1 / structural note
Landed 2026-07-08 (`docs/landings/2026-07-08-drag-capture-p2.md`). `overlay_contains_point`
tests the rect each overlay was *placed* at (`overlay_rects`, recorded in `build_overlays`),
which is exact for anchored/sized overlays like the Audio Setup panel (the BUG-059 case) but a
placeholder for the three `SelfManaged` overlays that draw their own footprint — dropdown,
browser popup, Ableton device picker. The dropdown is hit-tested accurately *upstream* in
`window_input`'s dropdown-dismiss branch (runs before the seam checks), so it is unaffected;
the residual exposure is only a browser popup or Ableton picker positioned directly over the
6px split band or 4px inspector edge, an unlikely layout. Burn-down: if a press-through bug
surfaces there in practice, give `SelfManaged` overlays a real footprint query (the D5-Deferred
§7 handle→widget-routing conversion also closes it). Not observed; carried.

### VD-019 — DRAG_CAPTURE P3: band-divider immediate-drag feel — L1 reached / L4 target
Landed 2026-07-08 (`docs/landings/2026-07-08-drag-capture-p3.md`). The zero-threshold
immediate-drag path for the audio panel's band dividers is proven L1 — a unit test drives a
`PointerDown`+1px `Move` on a divider and asserts `DragBegin` then `AudioCrossoverChanged`, and
a companion test proves a 3px wiggle on a normal surface still resolves to a `Click` (global
threshold untouched). What no test can reach is the feel: whether a ~2px crossover nudge tracks
naturally under Peter's hand with no sticky first-pixel lag. Owed as the design's stated L4 —
Peter nudges a crossover by ~2px live and confirms it tracks (D6 / §5 P3 performer gesture).
Burn-down: Peter's feel pass on the band dividers; no repeatable artifact substitutes for it.

### VD-006 — BUG-026 batch-2 popup entrance-tween fix: running-app confirmation — L2 reached / L4 target
Fix landed 2026-07-05 (commit `01c15213`) for the "no popup background until mouseover" bug —
root-caused as a missing animation-poll (see BUG-026). Gate green (clippy; `manifold-ui --lib`
604/604), but the headless `--script` driver has no frame loop and ticks `enter_anim` off
wall-clock, so it **cannot** exercise this timing bug. Burn-down: open the Add Effect browser in
the running app and confirm the dark background panel is present immediately, before moving the
mouse (and that the fade-in reads smoothly). Peter owns this L4 observation.

### VD-007 — PARAM_STORAGE P2 storage swap: GPU value-parity + live-app behavioural confirmation — L2 reached / L3 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p2.md`). The id-keyed apply path
(`apply_bindings` resolving by `source_id` instead of a positional index) is covered by 34
`param_binding` unit tests and the gpu-proofs suite COMPILES, but the GPU value-parity suite was
not run (P2 touches no shader/kernel/uniform). The three production behavioural fixes the test-pass
surfaced — revert-prunes-orphaned-user-params, calibration-reaches-the-renderer (D6),
gen-type-undo-restores-exact-arity — are unit/integration-tested but not exercised in a running app.
Burn-down: (a) `cargo test -p manifold-renderer --features gpu-proofs` for the GPU parity run;
(b) the running-app click-script in the landing report. Peter owns the L3 live observation.

### VD-008 — PARAM_STORAGE P3 transport topology guard: running-app confirmation — L2 reached / L4 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p3.md`). The transport bridge now stamps
each modulation block with `ParamManifest::topology()` and skips a block on apply when the live
topology no longer matches — closing the same-length-reorder misroute the old `len == len` guard
missed. Covered at unit level by the two `content_state::modulation_topology_guard_tests` (the exact
reorder-skip case + a control), but the *live* behaviour — a modulation display staying on the
correct slider when a neighbour param is deleted mid-modulation — is not exercised in a running app
(headless tests have no modulation loop / live UI). Burn-down: the running-app click-script in the
landing report (LFO on a slider, delete a neighbour, confirm the display stays put). Peter owns the
L4 observation. This is the one P3 gate step headless tooling cannot reach.

### VD-009 — PARAM_STORAGE P4 Ableton/OSC by-id resolution: real-hardware round-trip — L2 reached / L3 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p4.md`). Both live-hardware input paths
now resolve param mappings by manifest id against the live manifest, so user-added / glb-imported
params are mappable (Ableton) and addressable (OSC) instead of being silently dropped. Unit-proven:
the two repros, dispatch-by-id, and a guard that bundled OSC addresses are byte-identical to the old
positional derivation. Not exercised with real hardware. Burn-down: (a) map an Ableton macro to a
user-added / glb-generator param in the running app and confirm it moves; (b) send OSC to
`/master/{prefix}/{user_param_id}` and confirm the param moves, and that a bundled param's existing
address still lands byte-for-byte. Peter owns the L3 live observation.

### VD-010 — PARAM_STORAGE P5 inspector single-source: angle-card degree readout in a running app — L2 reached / L4 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p5-inspector.md`). `is_angle` now has a
single home on the manifest spec; an exposed angle param's card is proven to carry the flag through
the manifest + synth + JSON round-trip by unit test. Not observed rendering. Burn-down: in the graph
editor expose an inner `ParamType::Angle` param (or load a glTF and open its camera-orbit/tilt/FOV
card), confirm the card slider reads out `NN°` (not radians), and that a text edit round-trips
degrees↔radians without drift. Peter owns the L4 live observation.

### VD-011 — AUDIO_SENDS_UX P1 per-send gating: trace-count run with real audio — L1 reached / L2 target
Landed 2026-07-06 (`docs/landings/2026-07-06-audio-sends-ux.md`). The consumed-set walk is
unit-proven (4 tests on `Project::analysis_consumed_sends`) and the per-send skip is in the tick
path, but the doc's own P1 gate — `MANIFOLD_AUDIO_TRACE=1`, 16 sends, one bound param, log shows
"analyzed 1 send(s)" (2 with the scope open) — needs a running app with a capture device. The
instrument is shipped and env-gated. Burn-down: Peter (or a future L3 flow) runs the trace launch
per the landing report's click-script step 5. Peter owns the L2 observation.

### VD-012 — AUDIO_SENDS_UX P3 calibration drags: live feel + undo-step + no-capture-restart — L1/L2 reached / L4 target
Landed 2026-07-06 (same landing report). Drag arm/commit sequences and the dB/fraction math are
unit-proven (3 on_event tests); the layout and non-dim anchoring are PNG-verified (L2). Not
observed: meter following a gain drag against live audio, absence of capture-restart glitch, and
exactly one undo step per drag gesture — all inherently running-app. Burn-down: click-script step 4.
Peter owns the L4 feel-pass; it is the acceptance gate for the panel per the wave brief.

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

### VD-020 — CINEMATIC_POST P5/P6 (GTAO + AO denoise): Peter's look-pass not yet run — L1 reached / L2 target
Landed 2026-07-13 (`3e774a36`, AO-quality lane). Both phases are numerically proven (I7/I8's
named gpu_tests, generated-vs-hand parity, the negative `ssao_from_depth` deletion gate, a
migration round-trip test) and the orchestrating session reviewed before/after PNGs of
`CinematicScene` for gross defects (none found — see the landing report) — but per the doc's
§4 amended demo rule, an orchestrator PNG review is not a substitute for Peter's own verdict,
and the doc says so explicitly. Burn-down: Peter looks at the two PNG pairs named in the
landing report (or the live app) and confirms/rejects the look, especially GTAO's stated
honest cost (thin-object over-darkening) on a real scene rather than the demo's flat plane.

### VD-021 — GLB_CONFORMANCE G-P1+G-P2: Peter's look-pass on the AMG livery fix and card-curation UI not yet run — L2 reached / L4 target
Landed 2026-07-15 (`909976d2`). G-P1's conformance harness is numerically proven (4 `expect_pass`
checks green, goldens pinned at 2/255) and the orchestrating session viewed the DamagedHelmet
render, the held-out BoomBox/AntiqueCamera renders, and the AMG GT3 before/after (body livery now
renders silver/NASA instead of black — BUG-163 closed on this evidence). Per the doc's L2/L4
distinction, an orchestrator PNG review is not Peter's own verdict. Also unverified: the card
curation's UI surface (D4 — "the card shows 16 sliders, not 78" performer gesture) was proven only
at the `card_params`/round-trip level (`card_curation_caps_at_16_but_wiring_and_round_trip_stay_1_to_1`,
L1) — nobody has looked at the actual inspector panel on a >16-material import. Burn-down: Peter
drops the AMG (or another many-material glb) into a set live and confirms (a) the body livery looks
right, (b) the card shows exactly 16 sliders. Separately tracked, not blocking: BUG-165 (BoomBox
held-out asset never converges — a genuine gap found by G-P1's held-out gate, triage owed) and the
`TextureTransformTest` fixture gap (no glTF-Binary variant at the pinned commit — G-P4's problem).

*(VD-001–004 seeded 2026-07-05 from the memory corpus plus Peter's in-app findings; VD-006 added
2026-07-05, VD-007 at P2 landing, VD-008 at P3 landing, VD-009 at P4 landing, VD-010 at P5-inspector
landing. VD-005 closed at P2 landing. The full backfill pass over recent landings is still owed and
will extend this list.)*

## Closed

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

*(none yet)*
