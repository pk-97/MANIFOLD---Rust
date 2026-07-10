# Bug Backlog

<!-- index: Live, human-and-agent-facing tracker for known bugs not yet fixed. Each entry has a stable ID, a root-cause location, the user-visible symptom, a fix shape, and (when one exists) an #[ignore]'d test that goes green when fixed. -->

The repo had no bug tracker — bug knowledge lived only in agent memory, git history, and
session context. This file is the durable, in-repo home. It travels with the code, any agent
or human can read it, and it needs no external tool.

## How to use this file

- One entry per known bug, with a stable ID (`BUG-NNN`). Never renumber — IDs are referenced
  from commits, tests, and memory. (One historical exception: 2026-07-09 a duplicate `BUG-031`
  was split; the unreferenced audio-blip half became `BUG-081`.)
- **Status lives in one place: a `**Status:` line directly under each `### BUG-NNN` heading.**
  This is the single source of truth — the `## Open` / `## Fixed` section and the index table
  are *derived* from it, not authored in parallel (three copies of one fact is how this file
  drifted). Values: `OPEN` · `FIXED @ <sha>` · `PARTIAL` · `PARKED` · `DEFERRED` · `REOPENED` ·
  `SUPERSEDED`. `FIXED`/`SUPERSEDED` belong under `## Fixed`; everything else stays under
  `## Open` and in the index.
- **Tooling — `python3 .claude/hooks/bug_status.py`** checks the whole file for drift (a Status
  line that disagrees with its section, a resolved bug still in the index, an open bug whose
  named fix-design has SHIPPED, a duplicate id, an index row with no entry). `--write` inserts
  any missing Status lines and reflows entries into the right section behind a content-fidelity
  guard. The post-merge housekeeper (`design_status_check.py`) runs the same check and prints
  nudges — mirroring how design-doc statuses stay honest.
- The strongest form of an open entry is an **executable** one: an `#[ignore = "BUG-NNN"]`
  test that fails for the right reason. The bug is then self-documenting and self-closing —
  remove the `#[ignore]` when the fix lands and the suite enforces it forever.
- When you fix an entry, set its `**Status:` line to `FIXED @ <sha>` (add a **Fixed:** note on
  how) and run `bug_status.py --write` to reflow it into **Fixed**. Don't delete it — the
  history is the point.
- Severity is about the **instrument on stage**, not code aesthetics: `HIGH` = wrong output
  or silent data corruption a performer would hit; `MED` = reachable but narrow; `LOW` =
  latent / cosmetic / needs an unusual setup.
- **Escape analysis (added 2026-07-05):** a bug found in the app after an orchestrated
  landing carries one extra line in its entry — `Escaped: <wave/branch> · caught-by:
  <brief | gate | demo | held-out input | review>` — per `DESIGN_DOC_STANDARD.md` §10.
  Over time this is the empirical record of which orchestration stage leaks, so process
  fixes target the leaking stage instead of guessing.

---

## Index of open bugs (nickname → say this in chat)

| ID | Nickname | One line |
|---|---|---|
| BUG-086 | **recording-audio-track-under-covers-duration-on-longer-takes** | Repeated 2-minute 1920x1080 unpaced `recording-soak` self-checks measured `audio_duration_s` 1.3%-3.3% short of the intended duration (116.0s/118.4s/118.5s of 120.0s across three runs — variable, not a fixed percentage), while two independent 1-minute runs (1280x720 and 1920x1080) both measured exactly 60.0s — duration-dependent, not resolution-dependent, and not a "still queued at `stop()`" race (a 500ms settle delay before `stop()` changed the result by <0.1s). The native audio input silently drops on backpressure with no counter at all (`LiveRecordingPlugin.m:546-547`: `if (!state->audioInput.isReadyForMoreMediaData) return LR_OK; // drop samples rather than block` — unlike video's BUG-085, this path doesn't even log). Found 2026-07-10 building LIVE_RECORDING_PROOFS P2's soak self-check; root cause unknown (suspects: sustained real-time backpressure only manifesting past ~60-90s of continuous writing — disk I/O contention as the file grows, fragment-flush cadence, or AAC internal encoder buffering not fully flushed). MED — silent and uncounted, variable magnitude; unknown whether it scales worse over a full 20-minute take. |
| BUG-085 | **recording-frames-recorded-overstates-async-append-drops** | `LiveRecorder_EncodeVideoFrame` returns success (and Rust's `frames_recorded` counts it) as soon as the synchronous GPU blit into the CVPixelBuffer completes — but the actual `appendPixelBuffer:` call happens later, async, on `state->appendQueue`, and silently drops the frame (`"[LiveRecorder] VideoToolbox backpressure — dropped frame"`, no counter incremented) if `videoIn.isReadyForMoreMediaData` is false at that moment. Under heavy backpressure `frames_recorded` can overstate the file's real packet count by the async-drop count. Found 2026-07-10 building LIVE_RECORDING_PROOFS P1 (`pool_accounting_consistent`'s bounded-retry-recovery variant hit it once: 107 counted vs 106 actual packets). MED (accounting-only — the file itself stays valid, PTS stays monotonic; but a post-set frame count could read wrong). LOW in practice — the async drop needs genuinely sustained backpressure a real 60fps show submission rate is unlikely to hit. |
| BUG-083 | **video-export-has-no-progress-display** | Exporting video shows nothing until the finish toast — the content thread's per-10-frame progress snapshots never had a UI consumer (found 2026-07-09 by the A1 orphan lint; fields deleted, restore WITH a display from the P0 purge commit's parent). A multi-minute export looks like a hang. MED |
| BUG-084 | **recording-drop-counter-never-surfaced** | `recording_dropped_frames` (pool-exhaustion drops during live recording) was emitted every tick, read nowhere — a set-recording silently dropping frames is invisible to the performer. Surface on the recording indicator when non-zero; same restore path as BUG-083. LOW |
| BUG-082 | **trigger-fire-mode-level-features-near-dead** | The audio-mod drawer on a trigger/trigger-gate card offers all seven `AudioFeatureKind`s, but the fire chassis (`trigger_edge.advance` at 0.5 on the shaped signal) is tuned for impulses — Transients/Kick fire per hit; level features (Amplitude/Centroid/Flux/Pitch/Presence) cross mid once when the track gets loud and then sit disarmed, silently near-dead from the performer's view. Fix shape (AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION D6, lands P3): a live fire meter with the 0.5 threshold drawn as a line beside Amount on every fire-mode drawer — the engine honors level features as an invisible Schmitt trigger; visibility is what's missing. Feature restriction was considered and rejected (walks back LIVE_AUDIO_TRIGGERS U2). The separable widening (first-class level-crossing detector) is that design's Deferred #1 (MED) |
| BUG-080 | **param-manifest-construction-not-a-unified-safe-gate** | The param manifest (an instance's live knob list) is built at deserialize AND rebuilt by a later `reconcile_param_manifests` pass, because deserialize can't see project-embedded presets yet. Consumers that read `.params` *between* the two — a direct `serde_json::from_str::<PresetInstance>`, the keep-don't-drop backstop, the legacy audio-trigger migration, ~18 tests — depend on the deserialize-time build being correct. It works today only because the double-build papers over the timing; it's a latent hazard, not SOTA: a future load path added without a reconcile silently inherits an empty/partial manifest (the BUG-036 class). Root cause: manifest construction has no single safe gate — "partially built" is an observable, readable state. Fix shape (design pass, NOT a patch): make a half-built manifest un-observable — one construction gate every load/paste/bare-read passes through, OR a type-state where params can't be read until reconciled, OR deserialize carries enough context to build complete in one shot. The naive "build once in reconcile" was tried this session and is unsafe for exactly the reasons above (design doc §2 D1 priced + rejected it; see the 2026-07-09 double-build escalation). MEDIUM (design-quality / latent-robustness; wants an Opus design pass). |
| BUG-079 | **missing-preset-fails-silently-no-onscreen-signal** | Loading a project that references an unresolvable preset def (deleted, unregistered, or missing on this machine) degrades *safely but silently*: saved params are kept on a placeholder (keep-don't-drop, [`effects.rs:940`](../crates/manifold-core/src/effects.rs#L940)) and the effect falls back to **source passthrough** ([`preset_runtime.rs:808`](../crates/manifold-renderer/src/preset_runtime.rs#L808)) — but the ONLY signal is a console `eprintln`; nothing shows on screen. A performer sees the layer render without its effect (a missing *generator* layer likely renders empty — inferred, unconfirmed) with no visible reason. Fix shape: surface unresolvable presets in-app (a card/badge or a load-time notice). LOW |
| BUG-076 | **inspector-scroll-underestimates-content-height** | `layer_scroll`/`master_scroll`'s `max_scroll()` clamps to ~13-20px on a 9-card stack that's visibly ~1200px too tall for its viewport — the built content overflows but the scroll estimator doesn't agree (LOW, suspected root cause: `compute_height()` reads a mid-tween drawer-animation value instead of the settled/armed-at-build height) |
| BUG-074 | **audio-mixdown-flaky-under-parallel-tests** | `render_export_audio_tapped_layer_matches_rendering_alone` fails ~1-in-3 under default parallel `cargo test`, always green with `--test-threads=1`; unrelated to PARAM_STEP_ACTIONS (LOW) |
| BUG-073 | **ui-snap-script-drawer-tween-never-ticks** | `--script` harness has no per-frame tick, so a mod armed mid-script renders its drawer at a permanently zero-height clip region (unclickable rows) until the fixture pre-arms the state instead (LOW) |
| BUG-072 | **audio-mixdown-all-targets-clippy-debt** | Pre-existing `--all-targets` clippy failures in audio_mixdown.rs, unrelated to PARAM_STEP_ACTIONS, two one-line fixes (LOW) |
| BUG-046 | **low-band-kick-deafness-on-mixes** | Low=kick binding near-deaf on bass-heavy full mixes; HPSS measured DEAD 2026-07-06, successor = ridge-motion sweep event; partial (OR'd floored-novelty) on the shelf (HIGH) |
| BUG-047 | **setup-panel-overflow** | Audio Setup sections clip past bottom when a source has many input/consumer rows (LOW) |
| BUG-039 | **saw-rotation-wrap** | angle params clamp instead of wrapping; saw LFO can't spin a full rotation (MED, mechanism pinned) |
| BUG-045 | **gap-ring-down-chase** | tracker follows kernel ring-down down ~2-4 bins in note gaps; notes gate 87.6 vs 90 (LOW) |
| BUG-035 | **authoring-hitch** | ~59ms frame every ~5s: clip-atlas f16 convert on content thread (MED, root-caused) |
| BUG-037 | **glp-first-render-stall** | ~37ms warm-up on a glTF clip's first rendered frame (MED) |
| BUG-038 | **ableton-log-spam** | bridge warns every 1.5s forever when Live absent (LOW) |
| BUG-006 | **fused-param-noop** | param edits/undo on fused-away nodes silently no-op (HIGH) |
| BUG-007 | **fusion-exclusion-blind** | particle-loop exclusion misses configured wgsl_compute shapes (HIGH) |
| BUG-008 | **fused-buffer-oob** | mismatched array lengths read out of bounds in fused region (HIGH) |
| BUG-009 | **stateless-gate-miss** | harvest skip resets StateStore-held scalar state (HIGH) |
| BUG-010 | **wgsl-first-entry** | multi-entry wgsl_compute silently dispatches the first (MED) |
| BUG-011 | **fused-output-oversize** | fused output buffer sized to max of all inputs (MED) |
| BUG-015 | **inspector-overlap** | stale-chrome class FIXED 2026-07-08 — incremental cache path now falls back to full render on out-of-sub-region dirt (`has_dirty_outside_ranges` + `incremental_path_safe`); blanket `clear_dirty` narrowed to the overlay region so the fallback isn't erased. (2026-07-04 "sections interleaved" sighting = separate open thread if it recurs) |
| BUG-060 | **inspector-footer-overpaint** | REOPENED 2026-07-08. Opus 2nd pass: tree-geometry cause **ELIMINATED on the live cache path** (new `footer_leak_probe` test proves the inspector clips at footer_top through `traverse_flat_range`; footer's own render is correct) — the "inspector escapes into the footer" framing is wrong. Cause localized BELOW the tree, to the cache/dirty layer (tab-swap clears it = full recomposite). Artifact is **stale UI content** (UI colours / button fragments left behind), NOT clear/dark — the prior "footer goes dark, RGB 9-16" atlas dump was a HARNESS failure, not the symptom. Stale-pixel / dirty-clear bug, BUG-015 class. Needs live atlas+offscreen pixel dump. Cause still OPEN. |
| BUG-025 | **timeline-scissor-bleed** | clip content bleeds across row bounds (MED, repro needed — scrolled headless render 07-07 clean) |
| BUG-026 | **popup-fade-freeze** | fix landed, running-app verification owed (MED) |
| BUG-050 | **ableton-anchor-yankback** | play-from-cursor snap-backs; anchor fix landed, rig confirmation owed via [ABL-SYNC] logs (HIGH) |
| BUG-054 | **renderer-device-ptr-dangles** | renderers cache `*const GpuDevice` only `ContentThread::run()` repoints — any other consumer segfaults (MED, latent) |
| BUG-048 | **arm-two-reds** | ARM idle/armed both red, shade-only difference (LOW, UX call) |
| BUG-049 | **child-row-right-indent** | group-child right-anchored controls misaligned ~20px (LOW) |
| BUG-012 | **tex-rename-corrupt** | fragment `tex_` port-rename corrupts `tex_*` scalars (LOW) |
| BUG-018 | **catalog-stale** | node_catalog.json out of sync test red (LOW) |
| BUG-081 | **audio-load-blip** | ~10ms of audio leaks when a voice is built (LOW) |
| BUG-031 | **layer-menu-positional** | Layer context-menu + rename still address layers positionally (LOW, follow-up to the LayerId migration `877852a9`) |
| BUG-053 | **hdr-live-recording-structural** | HDR live recording can't work: pool format mismatches the native pixel buffer, nothing PQ-encodes (LOW today, blocks HDR capture) |
| BUG-034 | **atlas-uv-test-gap** | headless preview doesn't cover live atlas UV path (LOW) |
| BUG-014 / 030 | parked | NaN content-key hash · color-ratchet red |
| BUG-019 / 020 / 021 | deferred | group-fold gap · gen-card collapse · snap-back gap |
| BUG-056 | **audio-mixdown-clippy-debt** | `manifold-playback` clippy gate (`-D warnings`) fails pre-existing on `audio_mixdown.rs` — `cloned_ref_to_slice_refs` + `needless_range_loop` (LOW, blocks the crate's clippy gate, not correctness) |
| BUG-057 | **ui-snapshot-dead-blit-pipeline** | `cargo clippy -p manifold-app --features ui-snapshot` fails pre-existing on an unused `make_blit_pipeline` fn (LOW, blocks that one feature's clippy gate, not correctness) |
| BUG-063 | **silent-load-repairs** | PARTIAL — load-repairs now surface as a non-blocking "opened with repairs" toast (P3, no longer silent); the heavier rescue path (blocking ack dialog + journal the pre-repair project.json to history/) is deferred (MED-HIGH) |
| BUG-066 | **fluid3d-corner-drift** | FluidSim3D density herds into one corner (top-right at default params): turbulence noise is a wandering net tide + slope force has a sign-following, feather-scaled diagonal drift; root of the drift NOT yet found — 4 hypotheses refuted with evidence, harness in-repo (MED-HIGH, visible on stage) |
| BUG-067 | **ui-snapshot-dead-blit-pipeline** | `make_blit_pipeline` (`crates/manifold-app/src/ui_snapshot/render.rs:760`) is never used; `cargo clippy --features manifold-app/ui-snapshot -- -D warnings` fails on it, so any clippy run that chains the ui-snapshot feature (needed for `cargo xtask ui-snap` L3 flows) trips. Pre-existing at `b9304330`, found during DRAG_CAPTURE P1 (LOW) |
| BUG-068 | **inspector-scene-cliphit-overlap** | the `inspector` ui-snap scene fixture has a clip-vs-panel hit-test overlap at its narrower zoom — a clip can't be both uniquely-labeled and safely positioned over the inspector column, which forced DRAG_CAPTURE P1's L3 flow onto the `timeline` scene. Fixture-only, no runtime impact. Pre-existing at `b9304330` (LOW) |
| BUG-069 | **shipping-license-audit** | four license problems in shipped components: madmom models + ADTOF (both CC BY-NC-SA), rusty_link crate (GPL-2.0, viral, in manifold-playback), staged ffmpeg copied from the dev machine (likely GPL build); full sweep 2026-07-08, everything else clean (HIGH for commercialization, zero runtime impact) |
| BUG-070 | **stepper-and-nonstandard-slider-reset** | ~~decay drawer slider~~ + Clip Trigger drawer sliders now covered by the intrinsic-reset follow-through (@ 3a88f728, reset = required build input); **still open:** Audio Setup gain `[−]value[＋]` steppers + overlay-drag send-fader (not `BitmapSlider` tracks) (LOW) |
| BUG-071 | **ui-snap-dump-stale-parent** | `ui_snapshot::dump.rs` serializes `UINode.parent_id` (the mint-time struct field) instead of `UITree.parent_index` (the live array `reparent_root_nodes` actually mutates) — any node reparented via `ScrollContainer::reparent_content` (or the like) shows `parent: null`/its original parent in `--dump` JSON even though it's correctly clipped/nested for real rendering. Found 2026-07-08 verifying BUG-060: the dump made a correctly-fixed tree look unclipped, costing real debugging time before the PNG (the actual render) proved it was fine. Fix shape: either serialize `tree.parent_index[i]` in `dump.rs:38`/`:92`, or have `reparent_root_nodes` also update `self.nodes[i].parent_id` so the two stay in sync (LOW, dev-tooling only, zero runtime impact) |

## Open

### BUG-086 (recording-audio-track-under-covers-duration-on-longer-takes) — the recorded audio track can silently fall short of the intended duration on longer takes, no counter, root cause unknown — MED
**Status:** OPEN

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P2's `recording-soak` self-check gate
(`crates/manifold-recording/src/bin/recording_soak.rs`). Sequence: the soak bin's synthetic
audio was originally paced one chunk per video frame-loop iteration (media-time-locked); an
unpaced 4K/1080p run compresses many minutes of "media time" into a few seconds of wall time,
which triggered the native audio input's real-time backpressure gate
([`LiveRecordingPlugin.m:546-547`](../crates/manifold-recording/native/LiveRecordingPlugin.m#L546):
`if (!state->audioInput.isReadyForMoreMediaData) return LR_OK; // drop samples rather than
block`) and lost ~91% of the audio (10.8s decoded out of an intended 120.0s) — worse than
BUG-085's video path in one respect: this returns `LR_OK` (success) on drop and never logs
anything, so there isn't even a warning, let alone a counter. Root-fixed the soak's OWN pacing
(decoupled audio production from the video loop, paced to real wall-clock time instead, plus a
post-loop real-time catch-up phase — matches how production audio actually arrives, from a
real-time CoreAudio callback, never frame-coupled) which recovered the overwhelming majority of
the loss (10.8s → 118.4s of 120.0s). A **residual, VARIABLE shortfall remains and is
unexplained**: three repeated 2-minute 1920x1080 unpaced runs measured `audio_duration_s` at
116.0s, 118.4s, and 118.5s against an intended 120.0s (1.3%-3.3% short, run to run — not a
stable fixed percentage), while two independent 1-minute runs (1280x720 and 1920x1080) both
measured exactly 60.0s — so whatever's causing it is **duration-dependent, not
resolution-dependent**, onset is somewhere between 60s and 120s of continuous writing, and the
magnitude varies (possibly with system load/contention — not isolated). Ruled out: a "still
queued in the ring buffer at `stop()`" race — inserting a 500ms settle delay before calling
`session.stop()` changed the measured shortfall by <0.1s, so the loss is happening *during* the
run, not at shutdown. **Fix shape:** unknown without native-side instrumentation (out of P2's
scope — proof-harness/soak-authoring work, not native FFI investigation). Suspects: sustained
real-time backpressure that only manifests past some duration/data-volume threshold (disk I/O
contention as the fragmented MOV grows, fragment-flush cadence interacting with the audio
append queue, or AAC's own internal encoder buffering not being fully flushed by the periodic
drain before a threshold is crossed). Wire an `appendedFrameCount` counter for audio analogous
to BUG-085's video-side fix, or add NSLog-level visibility on the `LR_OK`-drop path at minimum,
so this stops being silent. Given the observed variance, the soak's own audio-coverage check
(`recording_soak.rs`, "Audio coverage sanity gate" comment) does NOT gate PASS/FAIL on a tight
tolerance — a made-up number would misrepresent confidence that doesn't exist yet — it gates
only a coarse 50% floor (catches a genuine collapse like the original 91%-loss defect) and
prints a non-gating stderr warning past 2% short, naming this bug. **Unknown whether this loss
scales worse over a full 20-minute take** — Peter's first full-scale soak run (P2's Deferred
item, per design §6 P2) will be the first real data point at show scale; if the shortfall grows
materially at that scale, this bug's severity should be revisited upward.

### BUG-085 (recording-frames-recorded-overstates-async-append-drops) — `frames_recorded` can overstate the file's real packet count under sustained backpressure — MED accounting / LOW practical likelihood
**Status:** OPEN

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P1's `pool_accounting_consistent` test
(`crates/manifold-recording/tests/recording_proofs.rs`), during a bounded-retry-recovery variant
that deliberately holds pool slots un-released to simulate a slow encoder. `session.stop()`
reported `frames_recorded: 107`; the file the harness's independent ffprobe oracle actually
opened had 106 video packets. Root cause, in
[`LiveRecordingPlugin.m`](../crates/manifold-recording/native/LiveRecordingPlugin.m) around
line 490: `LiveRecorder_EncodeVideoFrame` returns `LR_OK` (line 519) as soon as the *synchronous*
GPU blit into the CVPixelBuffer finishes — but the actual `[adaptor appendPixelBuffer:...]` call
happens **later, asynchronously**, on `state->appendQueue` (`dispatch_async`, lines 490-516).
Inside that async block, if `videoIn.isReadyForMoreMediaData` is false at the moment it runs
(real VideoToolbox backpressure), the frame is silently dropped —
`NSLog(@"[LiveRecorder] VideoToolbox backpressure — dropped frame at %.3fs", ...)` — with **no
counter incremented anywhere Rust can see**. Rust's `frames_encoded` (→
`RecordingResult::frames_recorded`) only reflects the synchronous return value, so it can never
observe this drop. The container file itself stays completely valid (PTS strictly monotonic,
no corruption) — this is purely an accounting gap: a post-set "N frames recorded" readout could
overstate the truth by however many frames VideoToolbox silently dropped under backpressure.
**Fix shape:** wire `atomic_int* appendedCounter` (already tracked at line 489, incremented at
line 500 on real success) back out through the FFI — e.g. a `LiveRecorder_AppendedCount(handle)`
query at `stop()`/finalize time, or have `LiveRecorder_Finalize`'s return value report the true
appended count instead of (or alongside) the synchronous-call count — and have
`LiveRecordingSession::stop()` prefer it. **Practical severity is LOW**: this needs genuinely
sustained `isReadyForMoreMediaData == false` backpressure, which the harness's artificial
fence-holding produces on purpose but a real 60fps show submission rate is very unlikely to
sustain (VideoToolbox's ProRes proxy encode is comfortably faster than realtime at these
resolutions). No `#[ignore]`-able regression test yet — `pool_accounting_consistent`'s current
gate (`frames_recorded + frames_dropped == frames_submitted_total`, tracked entirely
Rust-side) is internally consistent and doesn't touch this gap; a future test would need to
assert `probe(file).pts.len() <= frames_recorded` under intentional backpressure instead.

### BUG-083 (video-export-has-no-progress-display) — exporting video gives zero on-screen feedback until the finish toast — MED (export is a release pillar; long exports look like a hang)
**Status:** OPEN

Found 2026-07-09 (A1 orphan purge: the un-suppressed dead-code lint flagged `is_exporting` /
`export_progress` / `export_status` as never read; `git log -S` confirmed **no consumer ever
existed** — only the intro commit `d754eb08` and a lint sweep ever touched them). The content
thread's export loop faithfully sent progress snapshots every 10 frames into a void; the only
user-visible export feedback is the D17 finish toast (`export_finished`, which IS wired). From
the performer's view a multi-minute export is indistinguishable from a hang. **Fix shape:**
build the display (transport-bar strip or toast-with-progress), reading from re-added
`ContentState` fields — restore the emit side from the parent of the P0 purge commit
(`send_export_progress` in `content_export.rs` still runs as a keep-alive; its comment points
here). Per UI_PROJECTION_LAYER_DESIGN I1, the fields land WITH the consumer or not at all.

### BUG-084 (recording-drop-counter-never-surfaced) — live-recording dropped frames counted but never shown — LOW (gig-resilience visibility gap)
**Status:** OPEN

Same discovery path as BUG-083: `recording_dropped_frames` (fed by
`recording_session.frames_dropped()`, pool-exhaustion drops) was emitted every tick and read
nowhere. A recording silently dropping frames during a set is exactly the failure the performer
needs to see. **Fix shape:** surface it on the recording indicator (count or warning tint) when
non-zero; re-add the field with its consumer, emit side restorable from the P0 purge commit's
parent. Owner context: LIVE_RECORDING_PROOFS / gig-resilience territory.

### BUG-082 (trigger-fire-mode-level-features-near-dead) — fire-mode audio mods silently near-dead on non-impulse features — MED
**Status:** OPEN

Found 2026-07-09 (Peter noticed while discussing the Audio Setup redesign; mechanism confirmed in code same session). A fire-mode audio mod (`is_trigger`/`is_trigger_gate` target, §9-unified shape) evaluates *whatever* feature the user picks — [`modulation.rs:519`](../crates/manifold-playback/src/modulation.rs#L519) extracts the configured `AudioFeature`, shapes it, and edge-detects it rising through 0.5 via `trigger_edge.advance` — and the drawer's Feature row ([`param_slider_shared.rs:1574`](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1574)) offers all of `AudioFeatureKind::ALL` on trigger cards with no restriction or warning. The edge chassis (`TransientEdge`: fire at 0.5, re-arm hysteresis) is tuned for spike-and-decay signals: **Transients** and **Kick** fire per hit as intended, but level features (**Amplitude/Centroid/Flux/Pitch/Presence**) cross mid once when the track gets loud/bright and then sit disarmed until it drops — from the performer's view the trigger is silently dead or fires arbitrarily. (Non-obvious workaround that already works: the `rate_of_change` toggle differentiates a level into impulses.) **Fix shape (revised same day, on reconciling with LIVE_AUDIO_TRIGGERS §9 U2 "any feature, standard drawer" — a decided D that a feature restriction would walk back):** the engine *does* honor level features — they behave as a Schmitt trigger against the fixed 0.5 edge with Amount as the tune knob; what's missing is visibility, not capability. The fix is `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D6 (lands its P3): a live level meter with the fire threshold drawn as a line, beside Amount on every fire-mode drawer, so the crossing is tunable by eye. The separable widening — a first-class level-crossing detector (explicit threshold + hysteresis knobs) — is that design's Deferred #1 with a named revival trigger; Peter's call 2026-07-09: "'fire when amplitude crosses a level' is a real future widening but it's a detector question, separable from the config unification."

### BUG-080 (param-manifest-construction-not-a-unified-safe-gate) — manifest construction has no single safe gate; "partially built" is an observable state — MED (design-quality / latent-robustness; wants an Opus design pass)
**Status:** OPEN

The param manifest (an instance's live knob list) is built at deserialize AND rebuilt by a later `reconcile_param_manifests` pass, because deserialize can't see project-embedded presets yet. Consumers that read `.params` *between* the two — a direct `serde_json::from_str::<PresetInstance>`, the keep-don't-drop backstop, the legacy audio-trigger migration, ~18 tests — depend on the deserialize-time build being correct. It works today only because the double-build papers over the timing; it's a latent hazard: a future load path added without a reconcile silently inherits an empty/partial manifest (the BUG-036 class). Root cause: manifest construction has no single safe gate — "partially built" is an observable, readable state. **Fix shape (design pass, NOT a patch):** make a half-built manifest un-observable — one construction gate every load/paste/bare-read passes through, OR a type-state where params can't be read until reconciled, OR deserialize carries enough context to build complete in one shot. The naive "build once in reconcile" was tried 2026-07-09 and is unsafe for exactly those reasons (design doc §2 D1 priced + rejected it).

### BUG-079 (missing-preset-fails-silently-no-onscreen-signal) — an unresolvable preset def degrades safely but with no on-screen signal — LOW
**Status:** OPEN

Loading a project that references an unresolvable preset def (deleted, unregistered, or missing on this machine) degrades *safely but silently*: saved params are kept on a placeholder (keep-don't-drop, `effects.rs:940`) and the effect falls back to **source passthrough** (`preset_runtime.rs:808`) — but the ONLY signal is a console `eprintln`; nothing shows on screen. A performer sees the layer render without its effect (a missing *generator* layer likely renders empty — inferred, unconfirmed) with no visible reason. **Fix shape:** surface unresolvable presets in-app (a card/badge or a load-time notice).

### BUG-071 (ui-snap-dump-stale-parent) — `ui_snapshot::dump.rs` serializes the mint-time parent, not the live reparented one — LOW (dev-tooling only)
**Status:** OPEN

`ui_snapshot::dump.rs` serializes `UINode.parent_id` (the mint-time struct field) instead of `UITree.parent_index` (the live array `reparent_root_nodes` actually mutates) — any node reparented via `ScrollContainer::reparent_content` (or the like) shows `parent: null`/its original parent in `--dump` JSON even though it's correctly clipped/nested for real rendering. Found 2026-07-08 verifying BUG-060: the dump made a correctly-fixed tree look unclipped, costing real debugging time before the PNG (the actual render) proved it was fine. **Fix shape:** either serialize `tree.parent_index[i]` in `dump.rs:38`/`:92`, or have `reparent_root_nodes` also update `self.nodes[i].parent_id` so the two stay in sync.

### BUG-069 (shipping-license-audit) — four license problems in shipped components — HIGH at commercialization, latent until then
**Status:** OPEN

**Found 2026-07-08 (Fable, audio-analysis design session; full sweep same day — Python
runtime deps read from `requirements.runtime.mac.txt`, all Rust crate licenses swept via
`cargo metadata`, staging script read).** Peter's ruling, verbatim: *"Using for dev only
isn't good enough, what we build is what the users should have"* — every item below is
release-gating for the commercial cut, not optional.

1. **madmom model files — CC BY-NC-SA 4.0** (source is BSD; models say "commercial use
   requires contacting Gerhard Widmer"). Shipped via `bpm.py` / `onset_detection.py`.
   Fix in flight: AUDIO_ANALYSIS_ACCURACY P2 (Beat This, MIT code+weights) + P6 (full
   madmom removal), both with `rg 'madmom'` zero-hit deletion gates.
2. **ADTOF — CC BY-NC-SA 4.0** (code + model; we ship the `adtof-pytorch` port, which
   inherits it). Drum stage of the pipeline. **Peter's direction (2026-07-08): do NOT
   email Zehren yet** — replace instead. Full two-stage approach (DSP stem detectors
   now, own trained drum-stem model to compete — trained on demucs-separated permissive
   data, weights ours) is captured in AUDIO_ANALYSIS_ACCURACY_DESIGN Deferred #1;
   trigger = commercialization v1.0 gate or drum work resuming. Fresh off-the-shelf
   search when work starts (Magenta E-GMD model / Omnizart were the permissive options
   as of 2026-07, both mid).
3. **rusty_link 0.4 — GPL-2.0-or-later** (`crates/manifold-playback/Cargo.toml:17`,
   used by `link_sync.rs`). GPL is viral for a closed-source binary — this is the only
   non-permissive crate in the whole Rust tree. Ableton Link itself is dual-licensed
   and Peter's proprietary Link license is already pending (competitive-steal-pass),
   but that grant covers Link, NOT the community GPL Rust wrapper. Fix (Peter's
   direction 2026-07-08): once the Ableton proprietary Link license lands, write a
   thin clean-room FFI binding over Ableton's official `abl_link` C wrapper (~a day;
   `link_sync.rs` is the only consumer). **Never copy rusty_link source — copying GPL
   code inherits GPL.** Do not reimplement the Link network protocol; the licensed
   library carries it.
4. **Staged ffmpeg is whatever the dev machine has** (`stage_runtime_mac.sh:253–273`
   resolves `command -v ffmpeg` — a Homebrew build, i.e. `--enable-gpl`). Fix: stage a
   deliberate LGPL-configured decode-only ffmpeg build (the sidecar only decodes) and
   pin its source/offer per LGPL. The future app-side FFmpeg door (MEDIA_BACKEND)
   must pin the same constraint.

**Clean (verified 2026-07-08):** torch/torchaudio (BSD-3), numpy (BSD), demucs (MIT),
basic_pitch (Apache-2.0), librosa (ISC), soundfile (BSD), pretty_midi (MIT); every
other Rust crate is permissive (r-efi is MIT-or-Apache-or-LGPL — choose MIT). Minor
watch: the `lameenc.py` shim / LAME (LGPL — fine as subprocess/dylib, patents expired);
demucs htdemucs weight file license (⚠ verify at commercialization review). Datasets
are NOT affected — eval-only, never bundled.

COMMERCIALIZATION_DESIGN's license review must consume this entry wholesale.

### BUG-067 (ui-snapshot-dead-blit-pipeline) — dead `make_blit_pipeline` fails clippy under the ui-snapshot feature — LOW
**Status:** OPEN

**Found 2026-07-08 during DRAG_CAPTURE P1 gating; confirmed pre-existing** (present at base
`b9304330`, reproduced in a throwaway worktree; `git diff --stat b9304330 -- .../render.rs`
empty). Symptom: `fn make_blit_pipeline` at `crates/manifold-app/src/ui_snapshot/render.rs:760`
is never called; under `-D warnings` with the `manifold-app/ui-snapshot` feature the dead-code
lint denies the build. The no-feature clippy gate is clean, so it only bites a combined
`clippy --features ui-snapshot` invocation. Root cause: leftover helper. Fix shape: delete it,
or wire it if the blit path is meant to be used. Not blocking; outside DRAG_CAPTURE's file list.

### BUG-068 (inspector-scene-cliphit-overlap) — `inspector` ui-snap fixture clip/panel hit overlap — LOW
**Status:** OPEN

**Found 2026-07-08 during DRAG_CAPTURE P1 L3 authoring; pre-existing at `b9304330`.** The
`inspector` snapshot scene at its narrower zoom overlaps clip surfaces with the inspector
column, so no clip in it is simultaneously uniquely-labeled and safely draggable — which is why
P1's `drag-clip-release-over-inspector.json` flow proves position-independence on the `timeline`
scene (drag past the tracks' right edge) instead. Fixture-only, no app runtime impact. Fix shape:
adjust the `inspector` scene's clip layout or zoom so a clip clears the panel.

### BUG-066 (fluid3d-corner-drift) — FluidSim3D density herds into one corner; two causes isolated, one root still open — MED-HIGH (visible on stage in long-running clips)
**Status:** OPEN

**Found 2026-07-07 by Peter on the live output (subtle top-right dominance, no container and
cube container), bisected headless the same session.** Harness:
`crates/manifold-renderer/tests/fluid3d_bias.rs` (gpu-proofs, `--ignored`) — renders the
bundled preset 900 frames per scenario, prints per-quadrant luminance shares, dumps PNGs to
`/tmp/fluid3d_bias/`. Scenario matrix is edit-and-rerun (~12s/scenario at 512²); it injects
card params past their UI ranges (e.g. `curl 0`/`90`, `flow` sign flip), which the UI can't.

**Established (all at 512², cube container, deterministic):**
1. Baseline is clean: with turbulence=0, curl=0, flow=0 the steady state is symmetric
   (≈25% per quadrant) — spawn, NGP scatter, resolve, integrator, container repel/bounds,
   camera splat are not the bias.
2. **Turbulence is a wandering tide** (largest contributor at defaults): the 3-plane 2D
   simplex in `simplex_noise_force_3d_at_particles_body.wgsl` spans only ~2 lattice cells
   across the volume (`noise_pos = pos * 2.0`), so its instantaneous volume-mean is a real
   net force (measured ±0.05/axis in a CPU replica; drifts over ~30–60s as
   `noise_time = time*0.1` scrolls). Whole fluid leans on one wall, wall changes slowly.
   Fix shape: make the per-axis noise zero-mean over the volume (subtract the analytic/
   sampled mean, or raise frequency + add octaves — changes the look, needs Peter's eye).
3. **Slope force has a systematic diagonal drift — root cause OPEN.** slope-only (turb 0,
   curl 0, flow −0.01 default) pools 33–37% of luminance in the top-right with voxel-aligned
   "shelves"; at feather=40 it's a violent bulk translation (TR 50% by f300, striping against
   the +x face); at feather=4 the bias is gone. Flipping `flow` sign mirrors it to
   bottom-left; rotating the camera 180° mirrors it on screen (so sim-space, +x/+y-ward).
   The mean slope force over all particles is nonzero — for a pure density-gradient force
   that should be impossible (momentum conservation), so something in
   density→blur×3→gradient→curl_slope→blur×3→trilinear-sample is spatially shifted, and the
   shift grows with blur radius.
4. **Refuted with evidence (don't re-chase):** (a) NGP-deposit/trilinear-read PIC mismatch —
   a matched trilinear (CIC) 8-corner deposit in the scatter body changed the trajectory
   ~0.1% and the bias not at all; (b) Metal 8-bit subtexel rounding of the blur's bilinear
   tap-pair offsets — replacing pairs with exact integer-offset taps changed nothing;
   (c) codegen uv convention — the 3D wrapper uses texel-center `(id+0.5)/dims`
   (codegen.rs:168); (d) resolve index mapping — `id.z*dims.x*dims.y + id.y*dims.x + id.x`
   matches the scatter packing exactly. Also checked clean by read: gradient central-diff
   wrap, container SDFs, euler step, samplers (linear, clamp-to-edge).

**New evidence (Peter, 2026-07-07, after the session):** the pre-decomposition fused
`node.fluid_simulate_3d` (the original Rust generator, before the node-graph migration)
did NOT visibly have this bug. Unverified memory — verify it first (checkout a
pre-decomposition commit, run the harness's slope_only/feather40 scenarios against the old
generator, or eyeball a long run). If it holds, the root is something the decomposition
changed about the COMPOSITION, not the kernels — the per-kernel gpu_tests prove each atom
matches its legacy shader, so the delta must be in: pass ordering / which texture each pass
reads (stale-intermediate suspect, already ranked first below), double-buffering the legacy
frame did that the graph doesn't, blur radius units or pass count, gradient boundary
convention, or preset wiring/param scaling. Corroborating smell: the per-voxel curl wobble
(2211b20f) was added AFTER decomposition to fix "swirl pools in one octant" — pooling the
legacy sim reportedly never had, i.e. the wobble may have papered over one symptom of the
same introduced asymmetry. **This makes the first step a history diff:** `git log -S
fluid_simulate_3d` to recover the fused generator's frame recipe (kernel order, texture
ping-pong, per-pass params), then line it up against the FluidSim3D preset graph's execution
plan and diff the compositions.

**Legacy-composition diff (Fable, same night — step 0 partially done):** recovered the
pre-node-graph Rust generator (`git show 044e7c8a~1:.../fluid_simulation_3d.rs`) and the
fused-primitive era (`git show 50909419~1`). Force math is IDENTICAL through both eras
(same central-diff gradient, same cross(gradient, ref_axis) + slope, no wobble; fused
per-particle kernel = today's chain step for step), so the decomposition did NOT change the
physics. The composition deltas, complete list: (a) blur radius — legacy
`feather × vol_res/640` = 4 at defaults vs preset `feather × 0.2 + 1` = 5
(`blur_radius_scaled`); the harness cliff sits exactly there (radius ~1 clean, 5 visibly
biased, 9 violent); (b) legacy amortized the whole volume pipeline to alternate frames —
half-rate field updates ≈ half-speed drift; (c) legacy blur ping-ponged vol→temp→vol
explicitly (graph allocates per-pass outputs — check the executor actually does the same);
(d) the curl wobble exists only post-decomposition. Net: legacy likely drifted too, ~2–3×
slower (radius 4 vs 5 at the steep part of the response + half-rate updates) — consistent
with \"never noticed\" rather than \"absent\". So Peter's report probably does NOT convict the
decomposition; the drift mechanism itself (why any radius ≥ ~4 drifts diagonally) is still
the open question and the antisymmetry probe remains the way in. Verify (b)/(c) against the
execution plan before trusting this paragraph's \"identical\" claims for the blur pass wiring.

**Next steps (Opus):** the drift needs blur *range*, not blur sampling mode. Candidates, in
order: (0) finish the legacy diff — confirm the graph's blur ping-pong/order matches (c),
and if cheap, run the harness at radius 4 + half-rate to see legacy-equivalent drift speed;
(1) synthetic-volume antisymmetry probe at the kernel level — upload a symmetric
Gaussian density, run blur→grad→(slope)→blur, sample at mirrored probe positions with the
codegen standalone kernels (pattern: `gpu_tests` in scatter_particles_3d.rs), assert
F(p) = −F(mirror p) stage by stage; the first stage that breaks antisymmetry is the bug.
(2) The executor/fusion schedule for the 6-blur chain — if any blur pass reads a stale
(last-frame or not-yet-written) intermediate, symmetric math still yields a lagged, shifted
field; check the plan order + barriers for Render Volume/Force Field groups (this is the one
hypothesis that needs blur *range* to matter and survived every kernel-level check).
(1b) measure the violation directly instead of the drift: the per-particle force array is a
plain buffer — read it back in the harness and sum it. Nonzero mean force = the conservation
break itself, visible in ONE frame (vs 900-frame integration), and nulling force terms
attributes it to a stage instantly. Build this meter first; it makes every other probe 100×
faster. (1c) elimination note: vol_res 128 is a power of two, so blur/gradient coordinates
are EXACT in f32, and the integer-tap experiment made the blur reads exact too — drift
unchanged. The only position-dependent inexact ops left in the loop are the deposit
(refuted via CIC) and the per-particle trilinear read (2b below) — among precision theories,
2b is nearly the last one standing;
(2b) Peter's rounding hunch, sharpened (untested, fits ALL evidence): any precision theory
must round DIRECTIONALLY — symmetric noise can't pick a corner. Prime candidate: the per-particle force read in
`sample_volume_at_particles` — the one filtered read the integer-tap experiment did NOT
touch. Metal's trilinear filter fraction is ~8-bit; IF its rounding truncates (UNVERIFIED —
Apple documents the precision, not the direction; if it's round-to-nearest this candidate
dies), every read carries a fixed ~1/512-voxel shift toward −xyz and the feedback loop
amplifies it, and feather sets the force field's spatial COHERENCE
(radius 1 → decorrelated gradients, biases cancel; radius 5+ → aligned pushes compound) —
which explains the radius cliff without the per-read error changing size. Cheap test:
replace that one trilinear sample with a manual 8-tap textureLoad trilerp in f32 and rerun
slope_only — if the drift dies, root found (and the fix is exactly that manual trilerp);
(3) clamp-to-edge interaction at large sigma: blur clamps while the gradient wraps
toroidally — mixed boundary conventions couple opposite faces asymmetrically once the kernel
reaches the volume edge. Fix at whichever level the probe convicts; then rerun the harness
matrix (slope_only + slope_feather40 must go ≈25% flat) and give Peter a look-pass, since
zero-mean turbulence (item 2) changes the fluid's feel.

### BUG-063 (silent-load-repairs) — load-time repairs delete project data with log-only notice — MED-HIGH (silent data alteration; compounds BUG-062)
**Status:** PARTIAL

**Visibility shipped 2026-07-09 — PROJECT_FILE_INTEGRITY P3 (@ 05247ab1).** Load-time repairs
(unknown-effect strip, overlap-repair, orphan purge, missing-media) now accumulate a
`LoadReport` (a `#[serde(skip)]` transient field on `Project`) and, when non-empty, raise a
**non-blocking toast** naming what changed ("Opened with repairs: 1 unknown effect removed,
1 overlapping clip repaired"). The *silent* half of the bug — the core complaint — is closed.
**Still open (PARTIAL):** the heavier rescue path from the original fix shape — a *blocking*
acknowledge dialog AND journaling the pre-repair `project.json` into `history/` as a labeled
"before load repair" snapshot so the original is one restore away. Consciously deferred (design
Deferred §6); revival trigger: a repair found to drop data a user wanted back.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E2).** Three load steps mutate the project
destructively and report only to the log: `repair_overlapping_clips` (loader.rs:282) removes
the shorter clip of every overlapping pair, `purge_orphaned_references` removes clips and
MIDI mappings, `strip_unknown_effects` drops whole effects. The user believes they opened
the file they saved; the next save persists the altered state and the pre-repair original
ages out of the 50-autosave history cap. **Fix shape:** aggregate a `LoadRepairReport`
across the pipeline; any nonzero count raises a dialog naming what changed, and the
pre-repair `project.json` gets journaled into `history/` as a labeled snapshot ("before load
repair") so the original is one restore away.

**Correction 2026-07-09 (verified against the code, after conflating two mechanisms in chat).**
A **missing media file on disk does NOT remove any clip** — that was wrong when stated. Two
distinct things:
- `validate_clips` ([video.rs:118](../crates/manifold-core/src/video.rs#L118)) checks whether
  each clip's `file_path` exists on disk; a missing file is only **logged as a warning**
  ([loader.rs:207](../crates/manifold-io/src/loader.rs#L207)). Nothing is deleted. Move a project,
  break the paths → every clip stays put.
- `purge_orphaned_references` ([project.rs:1468](../crates/manifold-core/src/project.rs#L1468))
  removes a timeline clip only when its `video_clip_id` is **absent from the project's video
  library entirely** — a dangling internal reference, not a missing file. A clip whose file is
  missing on disk still has its library entry, so its id stays valid and the clip is kept. Purge
  fires only on structurally broken state normal authoring can't produce.
So the only load-time repairs that remove real content are `repair_overlapping_clips` (drops the
shorter of two overlapping clips — can't happen on projects saved by current builds, overlap being
a write-time invariant) and this dangling-reference purge. Peter's hard requirement — "missing
media must never delete a clip" — is **already the behavior**; the rescue-path priority drops
accordingly (a *relink* prompt for missing media would be the higher-value follow-up if any).

### BUG-076 (inspector-scroll-underestimates-content-height) — `try_inspector_scroll` clamps to a tiny max_scroll on genuinely tall content — LOW (found 2026-07-08 during UI_CLIP_AND_Z_OWNERSHIP_DESIGN P1)
**Status:** OPEN

**Symptom:** built a headless gate scene (`ui_snapshot/fixtures.rs`'s `bug060_scene`, added this
session) with 9 stacked effect cards, several with audio-mod drawers open — visibly, per the
unscrolled render (`target/ui-snapshots/bug060/bug060.png`), several cards extend well past the
1216px-tall canvas. Calling `UIRoot::try_inspector_scroll` (the same method
`window_input.rs`'s real mouse-wheel handler calls) with a delta of 300, 1000, or 1_000_000 all
converge to the SAME ~13-20px of movement and then stop — as if `max_scroll()` were computed as
roughly 20px, not the ~1200px the visible overflow implies.

**Root cause:** unknown — suspected but not confirmed. `ScrollContainer::apply_scroll_delta`
clamps against `self.content_height`, set via `InspectorCompositePanel::update_scroll_bounds`'s
`right_column_height()` -> `layer_column_height()`, which sums `card.compute_height()` per
effect card. Suspect: `compute_height()` reads a drawer-open-tween-animated height
(`drawer_height_anim`, see `param_card.rs`'s `drawer_open_tween_reserves_interpolated_height_
clips_then_settles` test) that starts at/near 0 and needs `tick_drawers(dt)` calls to reach its
settled value — a card configured with its audio mod ALREADY armed (as `bug060_scene` does, no
"click to open" step) renders its FULL drawer immediately (the build path uses the target
height directly) but `compute_height()` may still be reading the un-ticked animation state,
undercounting every card's height by its drawer's contribution. Not verified: whether
`configure()` seeds the animation at its target when armed from a cold build, or always starts
from 0.

**Fix shape:** instrument `right_column_height()`/`card.compute_height()` directly (a
`manifold-ui` unit test asserting `layer_column_height() ≈ sum of settled per-card heights` for
a 9-card, all-drawers-open fixture) to confirm or rule out the animation-state theory; if
confirmed, seed `drawer_height_anim` at its target value on first configure when the mod is
already armed (mirroring how the card already renders it), not just on a later toggle.

**Impact on this session:** blocked producing a scrolled-to-bottom PNG for
`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1's BUG-060 acceptance demo — worked around by deciding the
stopgap-removal question via a direct unit test (`InspectorCompositePanel::try_scroll_in_place`
called with a 1,000,000 delta, `manifold-ui`'s own suite, no PNG round-trip needed) instead of
the headless CLI harness. Also found and partially fixed en route, independent of this bug: the
L3 script runner's `Gesture::Scroll` never reached the inspector at all before this session
(routed only through the generic `UIEvent::Scroll` pipeline, which is real for the
dropdown/timeline but a no-op for the inspector's direct-call scroll path) — `script.rs` now
branches on `ui.layout.inspector().contains(center)` and calls `try_inspector_scroll` directly,
matching `window_input.rs`'s real dispatch. That fix is real and committed; this bug is what's
left after it.

### BUG-074 (audio-mixdown-flaky-under-parallel-tests) — a manifold-playback test fails intermittently only under the default parallel runner — LOW (found 2026-07-08 during PARAM_STEP_ACTIONS P3)
**Status:** OPEN

**Symptom:** `cargo test -p manifold-playback` (default, parallel) fails
`audio_mixdown::tests::render_export_audio_tapped_layer_matches_rendering_alone`
roughly 1 run in 3; `cargo test -p manifold-playback -- --test-threads=1` is
green every time (5/5 tried). Not caused by this phase's changes — the new
`param_step_clip_edge.rs` round-trip test (a different file, its own temp
path keyed by pid+nanosecond timestamp) isn't in the failure's module path.

**Root cause:** unknown — suspects: GPU-adjacent contention (audio_mixdown
renders via the export path, which may share a device/resource pool with
another test running concurrently) or a shared-mutable fixture. Not
investigated further — out of scope for PARAM_STEP_ACTIONS (audio_mixdown.rs
isn't part of that design, mirrors BUG-072's scope fence).

**Fix shape:** bisect by running audio_mixdown's tests alone in parallel vs.
serially interleaved with unrelated modules to isolate whether it's
intra-module or cross-module contention, then apply the standard fix
(dedicated resource per test, or `#[serial]`-style gating).

### BUG-073 (ui-snap-script-drawer-tween-never-ticks) — the headless `--script` driver has no per-frame animation tick, so a mod armed mid-script renders an unclickable, zero-height drawer — LOW (found 2026-07-08 during PARAM_STEP_ACTIONS P3)
**Status:** OPEN

**Symptom:** in a `cargo xtask ui-snap <scene> --script <flow>.json` run, a
click that newly arms a param's audio mod (or otherwise grows an EXISTING
card's drawer row count) dispatches correctly (confirmed via
`ui_bridge::dispatch` debug instrumentation — the right `PanelAction` fires
and mutates the project), but the drawer's own P1 reveal tween
(`ParamCardPanel::drawer_height_anim`, ticked by
`InspectorCompositePanel::update`'s `tick_drawers`) never advances: the
driver's `AutomationAction::Step` only increments a local `self.clock` field
used for input-event timestamps, nothing calls `tick_drawers`/`Panel::update`
with real elapsed time. The clip region sizing the reveal stays pinned at its
t=0 height (0, if the card is easing from unarmed) forever, so subsequent
rows in that clip region are invisible in the PNG AND unreachable by
`ui.pointer_event`'s hit-test (confirmed: `dump_tree_ex` still reports the
clipped nodes' raw, pre-clip rects with `VISIBLE | INTERACTIVE` flags, so the
dump looks fine while both the render and the click silently no-op — a
"dump says it's there, nothing else agrees" trap worth remembering before
trusting a dump alone against a freshly-armed drawer).

**Consequence for evidence-gathering:** any headless script that arms a
config-drawer-bearing param FOR THE FIRST TIME on an EXISTING card (one that
already went through one build with a smaller drawer) mid-script will show a
believable-looking `PNG`/dump pair with a truncated drawer. The workaround
used in `scripts/ui-flows/param-step-action.json`: pre-arm the mod directly
in the fixture (`ui_snapshot::fixtures::param_steps_scene`) so the card's
*very first* `configure()` call snaps `drawer_height_anim` straight to its
settled target (`param_card.rs`'s own comment: "a *new* param... snaps so it
never stalls half-open") — no tween in flight, no clipping. A REAL in-script
click that only changes content WITHIN an already-open, unarmed-row-count-
stable drawer (e.g. selecting a different Action/Mode segment on a param
that's already armed) is unaffected — confirmed working in the same flow.

**Fix shape:** either (a) give the `--script` driver a `self.rebuild`-adjacent
call that also ticks `ui.inspector`'s drawer/value-flash animations by a
large synthetic `dt` (e.g. `color::MOTION_MED_MS * 2.0`) after every
dispatch that sets `structural_change`, fully settling in one call instead of
requiring many small real-time-gated ticks; or (b) expose a
`skip_to_settled()`/`finish_all()` on `ParamCardPanel` the driver calls
unconditionally before every `Snapshot`/`Dump`/`Pointer`. Either closes the
gap for every future script that arms something mid-flow, not just this one.

### BUG-072 (audio-mixdown-all-targets-clippy-debt) — pre-existing lint failures in audio_mixdown.rs only visible under `--all-targets` — LOW (found 2026-07-08 during PARAM_STEP_ACTIONS P2)
**Status:** OPEN

**Symptom:** `cargo clippy --workspace --all-targets -- -D warnings` fails on
`crates/manifold-playback/src/audio_mixdown.rs:623` (`needless_range_loop`) and `:643`
(`cloned_ref_to_slice_refs`, `std::slice::from_ref` suggested). Confirmed pre-existing —
reproduces identically on main at `2682f9f4`, before any PARAM_STEP_ACTIONS change touched
this file.

**Root cause:** this codebase's standard gate command is `cargo clippy --workspace --
-D warnings` (no `--all-targets`), which never compiles integration-test binaries or exercises
lints inside them; these two lints only fire under the stricter `--all-targets` invocation,
which nothing had run against this file before. Found via the same stricter check adopted
this session after P1's `load_project.rs` compile break slipped through the non-`--all-targets`
gate.

**Fix shape:** two one-line clippy-suggested rewrites (`for (i, x) in tapped.iter().enumerate()`,
`std::slice::from_ref(&analysis_id)`); mechanical, no behavior change. Out of scope for
PARAM_STEP_ACTIONS (audio_mixdown.rs isn't part of that design) — left untouched per the
scope-fence rule.

### BUG-070 (stepper-and-nonstandard-slider-reset) — right-click reset still absent on the non-slider-track gain controls — PARTIALLY FIXED 2026-07-08 @ 3a88f728 — LOW (found 2026-07-08 during BUG-061)
**Status:** PARTIAL

**Update 2026-07-08 @ 3a88f728 (intrinsic-reset follow-through):** the envelope-decay drawer
slider is now wired (its `EnvDecay*` trio had a real handler, just no registration). More
importantly that commit made reset a *required build input* — `BitmapSlider::build` takes a
`reset` and returns `Slider { ids, reset }`, registered by one shared replay instead of per-panel
loops — which also closed the real motivating gap: the Clip Trigger drawer's Amount/Attack/Release
sliders (a trigger-gate row with no main slider, so the old per-panel loop bailed before reaching
them). **Still open:** the Audio Setup gain `[−]value[＋]` steppers and the overlay-drag audio
send-fader — neither is a `BitmapSlider` track, so both need a different gesture wiring (see fix
shape below); decide first whether right-click-reset is even the right affordance for a stepper.

**Symptom:** BUG-061 made right-click reset the shared gesture for every intent-registered
slider *track*, but three gain-ish controls don't render as a slider track and so were left
out: the Audio Setup gain `[−] value [＋]` steppers and the overlay-drag audio send-fader (both
in `audio_setup_panel.rs`), and the envelope-decay drawer slider (`param_slider_shared.rs`
`build_envelope_config`, which rides the same `drawer.rs` path but emits a different
value-change than the `AudioModShape*` trio BUG-061 wired).

**Root cause:** the steppers are a `[−]value[＋]` control (no track), the send-fader is
overlay-drag (`AudioSendGainDrag*`, not an intent-registered track), and the decay slider was
simply not in BUG-061's surface list. None expose the `SliderNodeIds.track` node that
`SliderReset` registration hangs off.

**Fix shape:** for the drawer decay slider, add a `SliderReset` on its track with the
envelope-decay default (mechanical, same as the other drawer sliders). For steppers/overlay
faders, decide whether right-click-reset is even the right affordance (they're not faders); if
yes, give the stepper a reset on its value cell and the send-fader a reset on its overlay hit
region — a different gesture wiring than the slider-track path. Not blocking; these all still
reset via drag-to-value.

### BUG-056 (audio-mixdown-clippy-debt) — `manifold-playback` fails `cargo clippy -D warnings` pre-existing on `audio_mixdown.rs` — LOW (blocks the crate's clippy gate, not correctness)
**Status:** OPEN

**Found 2026-07-07** while gating U-P1 of `LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 (the
`AudioTriggerMod` → `ParameterAudioMod` unification). Not this wave's fault: reproduces
identically on the wave's base tip (`8ccc4fc6`, verified via `git stash` + re-run before
touching anything) — a clippy-version-sensitive lint that was clean at whatever toolchain
last gated `manifold-playback`, now firing on unrelated code:

- `crates/manifold-playback/src/audio_mixdown.rs:589` and `:643` —
  `clippy::cloned_ref_to_slice_refs`: `&[normal_id.clone()]` / `&[analysis_id.clone()]` should
  be `std::slice::from_ref(&normal_id)` / `std::slice::from_ref(&analysis_id)`.
- `crates/manifold-playback/src/audio_mixdown.rs:623` — `clippy::needless_range_loop`: `for i
  in 0..tapped.len()` should iterate `tapped.iter().enumerate()`.

**Fix shape:** three mechanical one-line-ish clippy fixups in `audio_mixdown.rs`, no behavior
change. `cargo test -p manifold-playback --lib` is unaffected (tests still build and pass;
only `--tests -- -D warnings` fails). Not fixed here — out of scope for the audio-trigger
unification and touching `audio_mixdown.rs` wasn't part of this phase's brief.

### BUG-057 (ui-snapshot-dead-blit-pipeline) — `manifold-app --features ui-snapshot` fails `cargo clippy -D warnings` pre-existing on an unused fn — LOW (blocks that feature's clippy gate, not correctness)
**Status:** OPEN

**Found 2026-07-07** while gating U-P2 of `LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 (the trigger-gate
UI unification). Not this wave's fault — `crates/manifold-app/src/ui_snapshot/render.rs`
wasn't touched this session; `git log -S "fn make_blit_pipeline"` shows it landed in an
earlier, unrelated commit (`fea20ade`, "real per-node output thumbnails in the graph scene").
`fn make_blit_pipeline(device: &GpuDevice) -> manifold_gpu::GpuRenderPipeline` at
`ui_snapshot/render.rs:760` has zero call sites under any feature combination — plain
`dead_code`, not a lint-version regression like BUG-056.

**Fix shape:** either delete the function or wire it to its intended call site (unclear which
without reading the surrounding thumbnail-render code, out of scope for this phase). `cargo
build -p manifold-app --features ui-snapshot` and `cargo test -p manifold-app --lib` are both
unaffected (only `clippy --features ui-snapshot -- -D warnings` fails); plain `cargo clippy
--workspace -- -D warnings` (which doesn't enable the feature) stays clean. Not fixed here —
out of scope for the audio-trigger unification and touching `render.rs` wasn't part of this
phase's brief.

### BUG-054 (renderer-device-ptr-dangles) — renderers cache a raw `*const GpuDevice` that only `ContentThread::run()` repoints — MED (latent; every new headless/embedded consumer of ContentThread hits it)
**Status:** OPEN

**Found 2026-07-07 by the OFFLINE_AUDIO_REACTIVE_EXPORT P3 harness (first code path ever to
drive `run_export` outside the app's thread spawn).** `GeneratorRenderer` / `VideoRenderer` /
`ImageRenderer` cache a raw device pointer set at construction
(`generator_renderer.rs:126,180`); it dangles as soon as the owning `ContentPipeline` moves.
The running app is safe only because `ContentThread::run()` repoints every renderer once,
after all moves are complete (`content_thread.rs:300-328`) — a load-bearing, undocumented
ordering invariant. Any new consumer (headless export/journey harness, future preview
contexts, tests) that constructs a `ContentThread` and calls methods without replicating that
exact repoint gets an ObjC nil-receiver panic or a straight segfault, as the P3 build did
twice before finding the correct point. **Workaround shipped:** `journey_proof.rs`
`rebind_gpu_device_pointers` runs after the struct reaches its final binding — correct but a
second copy of the invariant. **Fix shape (root):** remove the self-referential raw pointer —
either pass `&GpuDevice` per render call (renderers already receive per-call context), or
hold the device behind a stable heap indirection owned above the pipeline so moves can't
invalidate it. Blast radius: renderer call signatures; no behavior change. Until then, any
brief that constructs `ContentThread` outside `Application::resumed()` must name the repoint
step.

### BUG-053 (hdr-live-recording-structural) — HDR live recording cannot work: pool format mismatches the native pixel buffer, and nothing PQ-encodes — LOW today (UI can't reach it), blocks any HDR-capture ambition
**Status:** OPEN

**Found 2026-07-07 by Fable during the LIVE_RECORDING_PROOFS design audit (statically
derived, not yet observed — no runtime repro attempted).** The recording texture pool is
unconditionally `Bgra8Unorm` (`crates/manifold-recording/src/session.rs:60`, comment
says "format conversion done in content thread"), but the native HDR path wraps its
CVPixelBuffer as `RGBA16Float` and blits pool → buffer
(`crates/manifold-recording/native/LiveRecordingPlugin.m:378`); Metal forbids blits
between 4-byte and 8-byte texel formats, so the first HDR frame should fail with
`LR_ERR_BLIT_FAILED`. Independently, the HDR writer config declares PQ/BT.2020 but no
stage in the pipeline applies a PQ transfer (the only converter is linear→sRGB,
`format_converter.rs`) — so even with matching formats the file would carry linear
values labeled PQ. Effectively the HDR path was never finished. **Stage impact today:
none** — the UI always records SDR (`app_render.rs:1257` uses `default_to_desktop()`,
hdr=false, and never sets the flag). **Fix shape:** pool format and converter must
follow `config.hdr` (Rgba16Float pool, PQ-encode compute stage or handoff of linear
values with correct color tagging — decide at design time), then replace the
`hdr_blocked_by_bug_053` guard test with the HDR twin of `nominal_video_only`, which is
this bug's acceptance test. See `docs/LIVE_RECORDING_PROOFS_DESIGN.md` §2 D7.

### BUG-050 (ableton-anchor-yankback) — Play-from-cursor: Ableton repeatedly snaps back to the gesture beat, then MANIFOLD clock-dragged after retries exhaust — HIGH (live transport; partial fix landed 2026-07-07, rig confirmation owed)
**Status:** PARTIAL

**Found 2026-07-07 by Peter, first L4 run of the ABLETON_TRANSPORT_SYNC wave (checklist
step 1).** Symptom: press play in MANIFOLD; Ableton keeps snapping back to the gesture
position (~once per retry interval); MANIFOLD's playhead holds for a few seconds (the
pending suppression working as designed), then snaps back when retries exhaust and MIDI
clock reasserts. Root defect (proven, fixed): the pending expectation froze its target
beat — the ack was a point match against a position both engines run away from, and
every retransmit re-seeked Ableton back to the stale anchor
(`transport_sync.rs`, fixed by the moving-anchor amendment — design doc deviation 5;
regression: `t5b`/`t7b` red pre-fix, `f8` pins the property). **Still open:** WHY acks
starved across several retries on the real rig — retry queries (`get/is_playing` +
`get/current_song_time`) should have acked by retry 1 even pre-fix, and the harness
cannot reproduce the starvation (its fake acks too fast in every plausible
configuration; see f8's honesty note). Suspects, unranked: real listener/query reply
latency under load; a reply-routing gap only manifesting live; beat-space offset
between MANIFOLD's timeline and Live's song time in Peter's set. **Oracle:** the
`[ABL-SYNC]` info logs added with the fix — gesture/retry/ack/degrade each dump the
observed snapshot (playing/song_time+age/tempo, or UNOBSERVED). One play-from-cursor
on the rig answers it. **Escaped:** ABLETON_TRANSPORT_SYNC wave, P2 stage — the
harness's FakeAbleton was fixture-overfit (instant first listener report, atomic
play+seek apply, prompt query replies); no scenario modeled a starved ack channel.

### BUG-049 (child-row-right-indent) — Group-child header rows double-pay the indent on right-anchored controls — LOW (visual misalignment, ~20px)
**Status:** OPEN

**Found 2026-07-07 by the label-collision fix worker (timeline-ux pass), verified in the
Liveschool after-PNG.** `layer_header.rs:489`: `handle_x = w - pad - HANDLE_W - 8.0` uses
`pad = PAD + CHILD_INDENT`, but the indent only moves the card's LEFT edge — so child
cards get a ~28px interior right margin vs 8px on top-level rows. Drag handles and Blend
chips sit ~20px left of their top-level siblings, and the collapsed name budget is 20px
tighter than necessary (it contributed to how early BUG-fixed label truncation kicks in).
**Fix shape:** right-anchored x's use `PAD`, not the indented pad. Moves rects pinned by
`layout_matches_frozen_oracle`, so it needs the oracle updated in the same commit — its
own small pass, not a drive-by. **Oracle:** the frozen-layout test + a child-row render.

### BUG-048 (arm-two-reds) — Automation ARM idle vs armed are both red, distinguished only by shade — LOW (stage-legibility; behavior-changing mode)
**Status:** OPEN

**Found 2026-07-07 (timeline-ux headless audit).** `transport.rs::automation_group`:
idle ARM = `RECORD_RED`, armed = `RECORD_ACTIVE` — a deliberate mirror of the REC
active/idle pair. But REC's two states are "recording or not", while ARM's decide what
touching a param DOES (override the lane vs punch automation INTO the arrangement) —
a wrong read on stage silently writes automation into the show. Headless renders show
the two reds are close at 1× (timeline vs automation scenes). **Fix shape:** give the
armed state a non-red or clearly distinct treatment (AUTOMATION_LINE_COLOR family per
the audit doc), or Peter rules the REC-pair consistency wins. UX call, not mechanical —
see `docs/TIMELINE_UX_AUDIT_2026-07-07.md` item 2.5. **Oracle:** the
`automation_state_toggles_update_styles_in_place` test pins current colors; it changes
with the fix.

### BUG-047 (setup-panel-overflow) — Audio Setup panel content clips past the bottom edge when chrome exceeds viewport − SCOPE_H_MIN — LOW (needs ~18 combined input/consumer rows on one source at full height; ~5 extra rows at a 720px window)
**Status:** OPEN

**Found 2026-07-06 during AUDIO_SENDS_UX P3 review** (orchestrated wave, found by the
worker's own analysis after an orchestrator-caught clipping defect was root-caused —
the clamp behavior below is the designed residue, not the bug that was fixed).
The panel sizes its spectrogram as `viewport − chrome_height()` floored at
`SCOPE_H_MIN` (200px). When a selected send's Inputs + Consumers rows (28px each)
push `chrome_height()` past `viewport − SCOPE_H_MIN`, the scope clamps at the floor
and the sections below it run past the panel's bottom edge — same visual as the
fixed P3 bug, different cause. **Symptom:** bottom consumer rows invisible on a
heavily-bound source. **Fix shape:** cap the consumers list at N rows + a "+N more"
summary row, or wrap the sections in the existing ScrollContainer (see
`guide_scroll_and_clipping` memory) — a deliberate UX call, not a mechanical fix;
don't improvise it inside an unrelated wave. **Oracle:** `audio_setup_panel.rs`
test `consumers_fit_within_panel_on_first_build_after_configure` guards the fixed
ordering bug; no executable test for this clamp overflow yet.

### BUG-046 (low-band-kick-deafness-on-mixes) — The canonical Low=kick binding is near-deaf on full mixes with active basslines — HIGH for the streaming/live-trigger use case
**Status:** OPEN

**Found 2026-07-06 (post-BUG-044 measurement, prompted by Peter):** on full mixes,
the Low band catches almost no kicks while Full catches plenty — bad_guy mix Low 6
vs drums-stem Low 46 (mix Full: 82); feel 7 vs 36; apricots 6 vs 13. inhale (29 vs
23) and tears (32 vs 26) are healthy — arrangement-dependent. Peter's use model is
per-band by design (Low = kicks/bass, Mid = vocals/synths, High = hats), so this is
the primary binding for kick-triggering being broken on bass-heavy genres.

**Mechanism (high confidence):** the Low band of a mix is where the sustained,
note-active bassline lives; the kick's low-frequency energy competes with the bass
IN the very band bound for it, keeping that band's ODF baseline (median AND recent
max) elevated. Full recovers kicks via their broadband attack click in mid/high —
which is why mixes fire well on Full but not Low. BUG-044's novelty criterion can't
help: bass notes are themselves novel events in the Low band.

**Fix direction (REVISED 2026-07-06 evening — HPSS-at-the-ODF measured and
exhausted; do NOT re-try it):** the P6a offline campaign
(AUDIO_OBJECT_TRACKING_DESIGN.md D9/P6; instrument kept at
`crates/manifold-audio/examples/hpss_proto.rs`, replica validated
fire-count-exact on all 25 fixtures) swept four causal families — column masks
(flutter manufactures ±59 dB events; growl 16–73 false fires), Wiener (dB flux
is scale-invariant; no effect), dB-novelty-floor replacement (collapses the
adaptive median's context; growl 0→62-73), OR'd floored-novelty (guard-green,
drums retention 1.00, apricots 5→12/13, feel 4→16/35, tears 8→12/25 — but
bad_guy 0→8/45). None reached the ~50% bad_guy bar; not integrated. **Measured
mechanism limit:** in a bass-occupied Low band the mix kick's surviving
evidence is its descending FM sweep (~2 bins/hop, plainly visible in the
bad_guy mix PNG crossing the bassline), which SuperFlux's max-filter nulls BY
DESIGN — no flux-family detector or threshold can recover it. **Successor
direction:** a percussive-sweep EVENT read from ridge motion (D5-tracker-
adjacent; v0 argmax-run prototype confirmed the signal exists but needs real
ridge tracking — apex sticks to the louder bass, bass portamento must be
discriminated by rate/extent, and cross-criterion refractory is needed or
attack+body double-fires). Needs its own short design; re-run the tracker gate
lines (extra Low fires feed D5 step 4 re-acquire). **Partial SHIPPED 2026-07-06 late @ `61c2b0fd`**
(Peter approved; masked-novelty third criterion in `reduce_send`; exact-match
gate vs the prototype 100/100, selftest green minus BUG-045's line): recovery
now apricots 12/13, feel 16/35, tears 12/25, inhale 17/22 — **bad_guy 8/45
keeps this bug OPEN** for the ridge-motion successor. Behavior change shipped
knowingly: Low transients also fire on bass-note attacks now. **Oracle caveat
(Peter, same night):** the drums-stem ground truth is ITSELF unverified
detector output — the bad_guy stem shows ~31 kick sweeps by eye but fired 46
times, so every recovery denominator is suspect until human labels exist. Do
NOT re-litigate recovery percentages against stem fires; the next pass grades
BOTH stem and mix detection against Peter's hand-labeled kick positions
(corpus incoming, Ableton-labeled — doubles as ingest P1's blocker). Also
settled in discussion: on-stage routing (drum bus as its own send) caps this
bug's priority for Peter's own sets — mix-only detection matters most for
finished/other-people's tracks; and the ridge-motion successor should track
the second ridge under the bass (the D5 machinery), not the argmax. Full-band is still NOT a substitute (hats spam —
Peter). Crossover-defaults sweep: independent report-only task; does not
address this bug (kick and bass share bins — re-confirmed at the bin level).

### BUG-045 (gap-ring-down-chase) — Tracker chases the transform's kernel ring-down during inter-note gaps — LOW (2.4 points on the notes gate; real-clip impact small)
**Status:** OPEN

**Found 2026-07-06 while fixing BUG-042** (its remaining accuracy misses after the
re-acquire-window fix). After every note release, the VQT's kernel memory presents a
DESCENDING salience artifact (energy decays slower in lower/longer kernels, so the
apex slides down: measured 149→144→133→118→100 Hz over ~6 hops on `notes`). The
early part of that slide moves at ≤ MAX_SLEW bins/hop, so continuation legitimately
follows it 2–4 bins down during the gap; the next attack then starts ~1–4 st low
until the onset re-acquire window rescues (~5 hops). Two partial guards shipped with
BUG-042: super-slew+moving continuation candidates are refused (hold instead of
clamp-chase), and a static super-slew peak in the MAX_SLEW..SLEW_RADIUS dead zone is
snapped to (tremolo-trough recovery). What remains is the sub-slew early chase.

**Oracle:** `P2c notes` accuracy line (87.6% vs gate 90 — the only known-failing
selftest line). **Fix direction (untried):** a value-trend discriminator —
ring-down decays ~0.90/hop at kernel rate while tremolo decays ~0.985/hop and a
real glide holds value — but that bar is a NEW tuned constant between two measured
distributions with ~2× separation, and a genuine fade-out slide (musical) sits on
the wrong side of it. Declined this session as knife-edge; needs either a
plateau-demonstrated sweep on real material or a smarter shape. Do NOT re-try:
raising SETTLE_STREAK (swept 2/3/4 — 69.2/87.6/86.1, K=3 is the plateau), or
re-clamping super-slew continuation (resurrects the 7-st gap-chase).

### BUG-039 (saw-rotation-wrap) — Angle params clamp at range ends, so a saw LFO / automation can't drive a smooth full rotation — MED (enhancement, performer-facing)
**Status:** OPEN

**Symptom** (Peter, 2026-07-06) — binding a saw LFO or an automation ramp to a rotation
param and sweeping 0→360° hitches at the wrap point: the effective value clamps at the
range end instead of wrapping, so continuous rotation — the most common motion move in a
VJ set — can't be played with a saw. Affects default card slider bindings across effects
and generators.

**Fix shape (mechanism pinned; Sonnet-executable, no design doc needed):**
- Add `wraps: bool` (serde default false) to `ParamSpecDef` — explicit tag, not inferred
  from `is_angle` (per `hidden-field-dependencies`; angle-typed ≠ periodic, e.g. FOV).
  Every existing project/preset loads unchanged.
- Apply wrap at the single point where modulation already post-processes effective values
  (where `whole_numbers` rounding lives): for wrapping params,
  `value = min + (v - min).rem_euclid(max - min)` instead of clamp. Base/undo semantics
  untouched — wrap applies to the effective only. Slider wrap-drag UX = later, not this pass.
- Mechanical sweep: every angle/degree-range param across primitive `ParamDef`s and the
  ~45 preset JSON card params; tag `wraps: true` ONLY where truly periodic (rotation,
  orbit, hue-angle, kaleidoscope angle). Clamped-for-a-reason params (FOV, ±89° tilt, arc
  extents) stay unwrapped. List every tag decision in the PR body.
- Gate: unit test on the wrap math (incl. negative saw), plus one preset smoke proving a
  saw 0→360 on a tagged param renders identical frames at phase 0 and phase 1.

**Sequencing** — AFTER the param-system post-refactor audit (Fable queue item 1): same
code region; land the audit's verified ground first.

### BUG-037 (glp-first-render-stall) — First render of a glTF scene layer stalls the content thread ~37ms (warm-up on the frame, not at load) — MED
**Status:** OPEN

**Symptom** — trace run 2026-07-06 (`meshImportTests.manifold`): the first frame after the
project's glp layer became active showed `generators=37.1ms` (RENDER_TRACE frame=421) —
one-off, distinct from the recurring BUG-035 spike. On stage this means launching a glp
clip mid-set drops ~2 frames on its first render.

**Root cause (probable, unmeasured beyond the one trace line)** — first-touch work in the
generator path: glTF texture decode hand-off / mesh buffer upload / pipeline+PSO creation
happens lazily on the first rendered frame instead of at load/schedule time. The repo
already has the machinery pattern for this class (`plugin_prewarm.rs`, generator pipeline
pre-warm at startup, pipeline archive).

**Fix shape** — pre-warm at project-load / clip-schedule time: when a glp generator clip
is loaded (or armed on a timeline), run its first-frame resource creation off the hot
path so frame 1 of the clip renders at steady-state cost. Verify with the same
MANIFOLD_RENDER_TRACE run: no >20ms frame on first clip render.

### BUG-038 (ableton-log-spam) — AbletonBridge retries + WARN-spams every ~1.5s forever when Live isn't running — LOW (log hygiene)
**Status:** OPEN

**Symptom** — any session without Ableton running logs
`[AbletonBridge] OSC send failed for /live/song/get/num_tracks: Connection refused` at
WARN level every ~1.5s indefinitely (see any 2026-07-06 trace-run log).

**Fix shape** — warn once on first failure, then downgrade repeats to debug until a send
succeeds (state flip logs "reconnected" at info). Optionally back off the poll while
refused. `manifold-playback/src/ableton_bridge.rs`, small.

### BUG-035 (authoring-hitch) — 3D scenes hitch when a camera/light param is animated — MED — re-encode hypothesis MEASURED AND REFUTED 2026-07-06; cause is app-side, still open
**Status:** OPEN

**Measurement (2026-07-06, Fable)** — `freeze-profile scene <glb> [param] [frames]` (new bench
arm): drives the production import door (`assemble_import_graph`) + production
`PresetRuntime::render` on the azalea fixture, static params vs `cam_orbit` swept per frame
(the LFO shape), with a convergence gate (async texture decode means the first ~120 frames
render black — un-gated numbers are void) and a sweep-sanity readback (min→mid must change
pixels; min→max on an angle param is a full circle, a no-op).

Results (600 frames/arm, converged, sweep verified live):
- **CPU encode of the whole chain: ~70µs p50, 0.35ms max, zero >1ms frames in 2400** —
  static or animated, 1080p or 4K. The "full-chain re-encode grazes the 16ms deadline"
  hypothesis is off by three orders of magnitude. Incremental command encoding would
  recover ~0.07ms/frame — **do not build it for this bug.**
- **No static-vs-animated delta**: CPU 0.067 vs 0.065ms p50 (1080p); GPU 2.23 vs 2.18ms.
  The graph runtime prices an LFO'd scene identically to a static one.
- Also refuted along the way: there is NO held-when-static gate at the compositor/layer
  level (the occlusion skip is blend-only — content_pipeline.rs "Everything still
  RENDERS"); the static-scene smoothness the original diagnosis leaned on comes from the
  executor's pure-step memo, and render_scene/gltf_mesh_source re-run every frame anyway.
- The mesh re-blit + per-object rebind "smaller shaves" live inside that 70µs envelope —
  not worth building for this bug either.

**Surviving suspects (all app-side, only run when a param animates):** the modulation/LFO
evaluator on the content thread; UI redraw driven by visibly-changing values (inspector
sliders, graph-editor canvas + thumbnail dump_set when the editor is watching); content↔UI
GPU contention (see `ui-present-content-gpu-contention` memory); present/pacing path.

**In-app profiler sessions (2026-07-06, Peter, `meshImportTests.manifold`)** — the hitch is
now precisely characterized: baseline content frame ~0.09ms, with **isolated single frames
of ~59ms (58.6/58.7/59.2), entirely inside `render_content_ms`**, cadence roughly one per
5–6s, present in BOTH the static and the LFO run. LFO/animation is fully exonerated as a
cause (the original framing was wrong — a static scene hitches identically; you just see it
when something moves). The quantized ~59ms magnitude + slow cadence says periodic
maintenance work or a blocking wait inside `render_content_native`, not render cost.
Candidate: `pool.prune_stale(300)` every 300 frames (content_pipeline.rs:1584-1595) — frame
indices of the spikes (900, 1233, 3630) are ≡ 0/33/30 mod 300, consistent if the pool's
counter is offset from the profiler's frame index. Unproven.

**CAUGHT (2026-07-06, MANIFOLD_RENDER_TRACE run)** — five of five spikes land in the
`clip_atlas` section: `clip_atlas=57.9–61.6ms`, cadence ~360 frames, exactly the
CLIP_ATLAS_SAVE_DEBOUNCE=300 cycle. The culprit line is
[content_pipeline.rs:2225](../crates/manifold-app/src/content_pipeline.rs#L2225) —
`clip_atlas_readback.try_read()` on the completed persist readback. `try_read`
([gpu_readback.rs:99-115](../crates/manifold-renderer/src/gpu_readback.rs#L99)) converts
f16→u8 **per pixel, per channel, scalar, on the content thread**, and the clip atlas is
8192×1152 Rgba16Float (75MB, 9.4M pixels) — ~58ms of CPU once per debounce cycle. The
section's "all disk IO is off-thread" claim is true; the CPU conversion before the
hand-off is the stall. (The separate one-off `generators=37.1ms` spike on the first
frame after load is glTF texture/pipeline warm — not this bug.)

**Fix shape (root: no O(surface) CPU work on the content thread)** — switch the persist
path to `try_read_packed()` (plain memcpy, gpu_readback.rs:148) and move the f16→u8
conversion + `slice_atlas_for_store` into the existing clip-thumb disk worker: hand it
(raw bytes, layout snapshot, hashes) and let it slice/convert/store on its own thread.
No new threads, no format change on disk.

**Symptom** — animating a 3D scene's camera or sun/light via LFO produces a slight, visible
hitch — an uneven frame spike, not a clean framerate drop. Reported by Peter 2026-07-05 on
glTF ("glp") scenes; suspected across all `render_scene` / 3D-mesh output. A static 3D scene
is smooth, and the *same* LFO on a 2D effect param is smooth (Peter confirmed 2D is fine).

**Root cause (hypothesis, reasoned from code — NOT yet measured)** — when a layer is dirty
it re-executes its whole effect chain, re-encoding every node's GPU commands into a fresh
command buffer each frame. There is no incremental "encode once, patch the changed uniform"
path. A static scene is held/composited without re-running the chain (this held-when-static
behavior is *inferred* from observed smoothness — the exact gate was not located in code and
should be confirmed during design). An LFO makes the layer dirty every frame, so the full 3D
chain re-runs 60×/s. That re-encode is the suspected fixed per-frame cost that grazes the
16ms deadline on the heavier 3D path while staying invisible on cheap 2D chains.

Confirmed by reading:
- `render_scene` and `gltf_mesh_source` are both non-pure (`PURE` defaults false,
  [primitive.rs:104](../crates/manifold-renderer/src/node_graph/primitive.rs#L104);
  neither overrides it), so the executor's memo-skip
  ([execution.rs:189](../crates/manifold-renderer/src/node_graph/execution.rs#L189)) never
  spares them — they re-run every frame the chain runs. The still-scene savings are NOT at
  the node-memo level.
- Per animated frame `render_scene` recomposes each object's model matrix, rebuilds its
  uniform struct, looks up the pipeline, and re-binds all 8 texture/buffer slots
  ([render_scene.rs:605-680](../crates/manifold-renderer/src/node_graph/primitives/render_scene.rs#L605-L680)),
  and `gltf_mesh_source` re-blits the whole mesh buffer
  ([gltf_mesh_source.rs:213-222](../crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs#L213-L222))
  even though geometry never changed.
- NOT the freeze compiler: render nodes are `Boundary` (non-fusable) and its recompile keys
  on structural content, "never per frame" ([freeze/install.rs:195-205](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L195-L205)).
  Exposed-param modulation flows as runtime uniforms and never changes the content key.

**Fix shape** — incremental command encoding for the graph runtime: cache a layer's command
buffer and only re-record when the graph *structure* changes, patching camera/light (and
other exposed) uniforms in place between frames. System-wide upgrade (every animated layer
benefits; payoff concentrated on expensive chains — 3D scenes, long stacks, many bindings).
Orthogonal to, and layers on top of, the existing memo system (skips pure nodes) and freeze
compiler (fuses pointwise passes) — an *addition*, not a rewrite. It sits on the hot render
path where a stale-uniform bug becomes the show, so this is HIGH-risk-to-touch. Smaller
shaves that reduce (not eliminate) the re-encode cost: persistent mesh buffer to kill the
per-frame re-blit; trim `render_scene`'s per-object rebind.

**Before building** — confirm the CPU re-encode is actually where the ms go: add per-frame
timing around the 3D chain execution and watch it under a running LFO. Steady ~X ms → render
cost, optimize the render; sawtooth → scheduling/overhead, and incremental encoding is the
fix. (Not run this session — the app isn't headless and Peter didn't want the round-trip.)

**Design owner** — queued to Fable for a proper design doc (`docs/*_DESIGN.md`), per
[[fable-priority-queue]]. Reasoned diagnosis only; verify the measurement first.

### BUG-081 — Audible blip when an audio clip's voice is built (play-then-pause leaks ~10ms of the file's start) — LOW
**Status:** OPEN

**Symptom** — a very subtle pop/click from the speakers at the moment an audio file is
loaded onto the timeline (e.g. Finder drag-drop). Reported by Peter 2026-07-05.

**Root cause** — [audio_layer_playback.rs:171-179](../crates/manifold-playback/src/audio_layer_playback.rs#L171-L179):
`make_voice` calls `manager.play(data)` at full volume and only then
`handle.pause(Tween::default())`. kira's `pause` is a fade-out — and `Tween::default()`
is a **10ms** linear fade (kira-0.9.6 `tween.rs:110`), not instantaneous — so the first
~10ms of the file renders audibly before the voice reaches its "start paused at 0" state.
Any file whose first samples carry signal produces the blip. (The 5ms `declick()` tween
used everywhere else in this module doesn't apply here; this is the one edge built on
kira's default tween.)

**Fix shape** — build the voice silent instead of pausing it after the fact: apply
`.volume(0.0)` to the `StaticSoundData` before `manager.play`, keep the pause+seek. The
per-tick sync path already restores the real volume via `set_volume(volume, declick())`,
so activation is unaffected. This kills the whole class including the race where an audio
callback fires between play and pause. One-line-ish, `manifold-playback` only.

### BUG-034 — Headless preview verification doesn't cover the live atlas UV path — LOW (test-coverage gap, follow-up to BUG-027)
**Status:** OPEN

**Gap** — the inline node-preview fix (BUG-027) is pixel-verified headless only through the
per-node-texture path (`ui_snapshot/render.rs`, whole-texture UV `[0,0,1,1]`). The LIVE app packs
every preview into one rotating atlas and samples a per-cell UV with letterbox/aspect trim; that
cell-picking math lives inline in [app_render.rs](../crates/manifold-app/src/app_render.rs) and is
NOT exercised by any headless render (the atlas is filled by the content thread). So a subtle cell
or aspect error would show wrong/offset/squashed previews in the running editor but pass every test.

**Fix shape** — (1) factor the atlas-cell-UV math out of `app_render.rs` into one shared helper;
(2) in the harness, pack the already-rendered per-node textures into a synthetic atlas + build the
matching `node_atlas_layout`, register it under the atlas handle, and drive previews through that
shared helper. Then a single graph PNG proves the live cell math, not a copy of it. Not large.
Gated behind BUG-033 (the `ui-snapshot` harness doesn't compile on trunk).

### BUG-030 — Design-token ratchet red on trunk: raw `Color32::new(` count 201 vs baseline 200 — LOW (parked, not param-storage)
**Status:** PARKED

**Root cause** — a UI landing added one raw `Color32::new(` literal in `crates/manifold-ui/src`
without tokenizing it or bumping the ratchet. [design_tokens.rs:40](../crates/manifold-ui/tests/design_tokens.rs#L40)
sets `COLOR_BASELINE = 200`; the actual scan count is 201.

**Symptom** — `cargo test -p manifold-ui --test design_tokens` fails (`no_new_raw_color_literals`,
201 > 200). **Fails identically on origin/main (58bc2d43)**: `crates/manifold-ui/src` is
byte-identical between that commit and the P2 branch, and `scan()` reads only that directory, so
the drift predates and is independent of P2.

**Found during** — PARAM_STORAGE P2 (2026-07-05), full-workspace sweep after merging origin/main.
Two pre-existing trunk failures surfaced (this + the stale node catalog, which P2 regenerated) —
a signal that a recent UI landing skipped the full workspace test.

**Fix shape** — the UI/design-token owner tokenizes the offending literal (a `color::` token, or
`// design-token-exempt: <reason>`); the ratchet then returns to green at 200. Left red on purpose
rather than bumping the baseline, which would silently bless the drift the ratchet exists to catch.
Unrelated to param storage.

BUG-006–014 come from the **freeze-compiler adversarial bug hunt, 2026-07-03**
(40-agent Sonnet workflow `wf_73bb4ddf-885`; 10 finder lenses → every finding attacked by 2
independent skeptics). BUG-006–012 were **confirmed by both skeptics** with line-level
evidence; BUG-013/014 got split verdicts (judgment recorded per entry). Full verifier
transcripts: the workflow journal at
`~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/18511d71-15ae-4119-81cc-894a3f83d247/subagents/workflows/wf_73bb4ddf-885/journal.jsonl`.
System context for all of them: [FREEZE_COMPILER_MAP.md](FREEZE_COMPILER_MAP.md).

### BUG-006 — Param edits/undo on fused-away nodes silently no-op until an unrelated rebuild — HIGH
**Status:** OPEN

**Root cause** — [bound_graph.rs:114-133](../crates/manifold-renderer/src/node_graph/bound_graph.rs#L114-L133):
`apply_inner_param_overrides` looks each node's `node_id` up in `slot.node_map` and silently
`continue`s on a miss. For a fused card, `node_map` is built from the FUSED def
([preset_runtime.rs:1285-1288](../crates/manifold-renderer/src/preset_runtime.rs#L1285-L1288)),
so fused-away members (e.g. `gain`) aren't in it. The path never consults the fused view's
`fused_retarget` map (which knows `gain.gain` → `fused_region_0.n0_gain`). Value-only edits
bump only `graph_version`, which is deliberately not in `compute_topology_hash`, so no rebuild
fires.

**Symptom** — edit a param in the editor, close it (re-fuses, bakes the value), then Undo
while viewing another effect: the def reverts but the fused kernel keeps rendering the OLD
value indefinitely, until a resize/editor-open/unrelated edit forces a rebuild. Live control
stranded, zero errors. `CHAIN_FUSION_DESIGN.md` §6 already flags this as an open item.

**Fix shape** — thread the fused view's `fused_retarget` into `apply_inner_param_overrides`
(or into `node_map` construction): on a `node_map` miss, translate `(node_id, param)` through
the retarget map to `(fused node, n{i}_field)` and apply there. Test: fuse, value-edit,
assert the fused node's param moved without a rebuild.

### BUG-007 — Particle-loop fusion exclusion is blind to configured `node.wgsl_compute` shapes — HIGH
**Status:** OPEN

**Root cause** — [region.rs:834](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L834):
`cycle_contains_array` uses a bare `registry.construct(type_id)` — the ONE hold-out in the
file; every other classification call site uses `configured_construct`, whose own doc comment
states why the bare form is wrong. A full-kernel `node.wgsl_compute` with a
`var<storage, read_write> array<Particle>` output (StrangeAttractor's "simulate" node is a
shipped instance) introspects as the DEFAULT kernel (no Array output) under the bare
construct, so the cycle scan can't see the particle stage.

**Symptom** — a texture atom on a feedback loop whose only Array producer is such a node
passes cut rule 12 and fuses tier-A f16 in-loop, where the bit-exact induction argument does
not hold across a particle/scatter stage (FluidSim precedent: max_abs ~0.73 over ~31% of
pixels). Fused render visibly diverges from the editor.

**Fix shape** — one line: use `configured_construct(registry, node)` in
`cycle_contains_array`. Sweep the file for any other bare-construct hold-outs
(`node_is_buffer_atom` / `region_is_buffer` at
[region.rs:1885-1905](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L1885-L1905)
have the same pattern — audit while there). Test: a loop through a configured wgsl_compute
particle node must classify its texture atoms Boundary.

### BUG-008 — Fused buffer region with mismatched array lengths reads out of bounds — HIGH
**Status:** OPEN

**Root cause** — [codegen.rs:1777-1813](../crates/manifold-renderer/src/node_graph/freeze/codegen.rs#L1777-L1813):
`generate_fused_buffer` anchors the dispatch guard to the FIRST array external's
`arrayLength`, then unconditionally pre-reads EVERY array external at that index. Nothing
anywhere (classify, union, `build_region`, `fused_def_builds`) checks that a buffer region's
array externals agree on length — the tier-6 uniformity gate is texture-only. The unfused
atom (e.g. `LerpInstanceFields`) explicitly clamps to `min(a_cap, b_cap, out_cap)`.

**Symptom** — two array inputs of different lengths fuse; for indices past the shorter
buffer the kernel does an out-of-bounds Metal storage read and writes garbage
instances/particles to the output — silent visual corruption. Shipped presets happen to share
lengths today; user graphs are unprotected.

**Fix shape** — either refuse at `build_region` when a buffer region has >1 array external
(conservative, fail-closed, cheapest), or emit a per-external in-bounds guard
(`idx < arrayLength(&src_e)` with a defined fallback element). Pair with BUG-011.

### BUG-009 — Segment "stateless" gate misses StateStore-held scalar state; harvest skip resets it — HIGH
**Status:** OPEN

**Root cause** — [segment.rs:153-171](../crates/manifold-renderer/src/node_graph/freeze/segment.rs#L153-L171):
`def_is_segment_stateless` checks only `state_capture_input_ports` + `aliased_array_io`.
Primitives that hold real cross-frame state in the StateStore without declaring either —
`sample_and_hold`, `envelope_decay`, `trigger_ease_to`, `compressor_envelope`,
`envelope_follower_ar`, `inject_burst` — pass as stateless. Segment member slots get
`def_content_key: 0` ([preset_runtime.rs:1105](../crates/manifold-renderer/src/preset_runtime.rs#L1105))
and `harvest_state_from` skips them
([preset_runtime.rs:1693](../crates/manifold-renderer/src/preset_runtime.rs#L1693)), so any
chain rebuild drops their state.

**Symptom** — AutoGain (shipped: `compressor_envelope` next to pointwise atoms) joins a
segment; any rebuild while it's a member — editor open/close elsewhere, an unrelated card
edit, or the fused-segment swap-in itself — resets the envelope: gain snaps to unity, a
visible/audible pop mid-show. Violates the chain-fusion design's own "never resets state"
invariant.

**Fix shape** — the root fix is a truthful statefulness signal: a `NodeRequires`-style
`uses_state_store` flag (or derive it from `ctx.state` usage) that `def_is_segment_stateless`
also checks. Stop-gap is a hard-coded exclusion list, which is exactly the pattern the freeze
module refuses everywhere else — prefer the flag.

### BUG-010 — `wgsl_compute` silently dispatches the first of multiple entry points — MED
**Status:** OPEN

**Root cause** — [wgsl_compute.rs:615-624](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L615-L624):
`introspect()` takes `module.entry_points[0]` with no `len() == 1` check (the module doc at
lines 29-31 claims multiple entry points fail validation — they don't). The pipeline compile
independently picks the same first entry. A fragment-form node embeds the author's raw text
BEFORE the synthesized `cs_main`, so any leftover `@compute fn` in the fragment becomes
entry 0 and is what actually runs. Verified empirically by a skeptic (scratch test:
`compile_failed=false`, `debug_pass` dispatched, real kernel never runs).

**Symptom** — a user kernel/fragment with a stray second `@compute` function (debug leftover,
copy-paste) renders stale/blank output with no warning; downstream wires read it as if it
worked. Authoring-time surface, so MED — but it's the exact silent-wrong-output class.

**Fix shape** — in `introspect()`: if the module has >1 compute entry point, prefer `cs_main`
by name; if absent, fail validation with the warning the doc already promises. Keep the
dispatch-side pick in lockstep.

### BUG-011 — Fused `@fused_output` buffer sized to max of ALL array inputs, not the member's own rule — MED
**Status:** OPEN

**Root cause** — [wgsl_compute.rs:1828-1829](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L1828-L1829):
the fresh-output branch of `array_output_capacity` returns
`input_capacities.max()` generically, overriding the fused output member's own semantic
capacity rule (e.g. `LerpInstanceFields` follows only input `a`). Downstream consumers
(`render_instanced_3d_mesh` computes capacity from physical buffer size) can then draw ghost
instances from the never-written tail.

**Symptom** — with mismatched input lengths (same shape as BUG-008), the fused output buffer
is larger than the unfused chain's, and its tail is uninitialized pooled VRAM — potential
stale-data ghosting across preset/frame boundaries.

**Fix shape** — falls out of BUG-008's decision: if multi-external buffer regions are
refused, this is unreachable; if guarded instead, size `dst` from the anchor external and
zero-fill or guard the tail.

### BUG-012 — Fragment `tex_` port-rename corrupts scalar params named `tex_*` — LOW
**Status:** OPEN

**Root cause** — [wgsl_compute.rs:544-548](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L544-L548):
the fragment-form rename loop strips a literal `tex_` prefix from EVERY input port name with
no type filter (the sibling texture-binding rename at 549-561 IS filtered to
`SampledTexture`). A scalar `@param: tex_speed` exposes port `speed` while the uniform layout
and params stay keyed `tex_speed`; the dispatch-time wire lookup misses and the live wire is
silently ignored.

**Symptom** — a wired LFO/Ableton control on such a param renders as connected but never
moves the value. Latent — no shipped preset uses a `tex_`-prefixed param name.

**Fix shape** — filter the rename to texture-typed ports, mirroring lines 549-561. One-line.

### BUG-014 — Content key collapses NaN/±Inf param values to one hash — LOW (parked)
**Status:** PARKED

**Root cause** — [install.rs:205-215](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L205-L215):
`def_content_key` hashes `serde_json::to_vec(def)`, and serde_json writes non-finite floats
as `null`, so defs differing only in a non-finite param share a key while the fuse bakes the
raw f32.

**Status** — split verdict, judged UNREACHABLE today: the second skeptic traced every write
path into node params (scrub handlers clamp to finite ranges; JSON round-trips reject
non-finite). Parked as a hardening note — if a new param write path ever skips the clamp,
this becomes live. Cheapest closure: reject non-finite values at the `SerializedParamValue`
boundary (the eliminate-bug-class-at-storage-layer pattern).

### BUG-015 — Inspector sections render overlapping / at stale offsets after scroll — MED (repro needed)
**Status:** OPEN

**Symptom** — observed once by Peter, 2026-07-04, right after the timeline-P0 / multi-select
UX changes landed: the layer inspector drew its sections interleaved — the MIDI block
(MIDI / CHANNEL / DEVICE) and the audio-send block (send dropdown, +0.0 dB) overlapping
each other with a dead band between them, and the "No audio input" header clipped mid-panel.
Described as "a scrolling bug with the UI timeline updates". Screenshot lives in the
2026-07-04 session transcript.

**Root cause** — unknown. Suspect surface: inspector section Y-layout vs. scroll offset
(the `single-source-y-layout` invariant) or a stale subregion scissor
(`subregion-scissor-invariant`) going stale when timeline updates force a rebuild while the
inspector is scrolled.

**Repro** — not yet pinned. First step is reproducing: select a generator layer, scroll the
inspector, then trigger timeline churn (clip drag / multi-select updates) and watch for
section overlap.

**Fix shape** — TBD after repro. If it's the known invariant class, the fix is at the layout
single-source, not per-section patches.

**Repro attempt 2026-07-07 (timeline-ux headless audit)** — scroll-seeded `states` render
(101px) + driven inspector scroll on the `inspector` scene: sections stay correctly laid
out in both. Not reproduced. The missing ingredient per the symptom is timeline churn
DURING a scrolled state (rebuild-while-scrolled); the `--script` driver can now interleave
scroll + clip-drag + snapshot in one flow (post real-dispatch fix, this branch), so a
dedicated repro flow is now writable when this bug is next picked up.

**Sighting + concrete progress 2026-07-07 (Opus session)** — Peter hit inspector
artifacts again: on a Fluid Simulation generator (Master tab), stale fragments at the
panel's left edge (one is a patch of viewport/video showing through) plus a clipped sliver
above the Layer/Master tab strip. Screenshot in this session's transcript. May be this bug
or a close sibling — same suspect surface (stale inspector content), same repro difficulty.

_Ruled out this session:_
- NOT the just-merged trigger-gate drawer (§9): the drawer is CLOSED in the repro
  screenshot. Two proposed mechanisms for it — a "Mode row" escaping its clip parent, and an
  unbalanced Overlay paint-layer push — are both refuted by the code (every node in
  `build_toggle_trigger_row` parents to one `parent`; there is zero paint-layer manipulation
  in the card/drawer path).
- NOT a settled-state containment error: built the armed trigger-gate card into a real
  `UITree` and measured — one root node, max node bottom == the card's reserved height
  exactly, zero overflow. Height accounting (incl. the Mode row via `audio_config_height(true)`)
  is exact.

_New concrete suspect (stronger than the two above):_ the inspector's incremental atlas
cache. `UICacheManager::render_dirty_panels`
([ui_cache_manager.rs:175](../crates/manifold-renderer/src/ui_cache_manager.rs#L175))
repaints only dirty CARD sub-regions and trusts `LoadOp::Load` for everything else. The
sub-regions are the cards only
([inspector.rs:506 `sub_region_ranges`](../crates/manifold-ui/src/panels/inspector.rs#L506)) —
section backgrounds, tab strip, padding, inter-card gaps and margins sit in NO sub-region, so
an incremental frame never repaints them and a stale pixel there survives until the next full
render. The guard `extents_unchanged`
([:282](../crates/manifold-renderer/src/ui_cache_manager.rs#L282)) approximates each card's
painted extent by its FIRST node's bounds, so anything a card paints OUTSIDE its frame is
untracked.

_Measured seed candidate:_ `build_toggle_trigger_row`
([param_slider_shared.rs:1532](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1532))
lacks the `drawer_reveal` reveal-clip that `build_param_row` has
([:2005-2017](../crates/manifold-ui/src/panels/param_slider_shared.rs#L2005)), so its drawer
paints ~120px below the card frame, unclipped, for the whole open/close tween (measured:
0 clip regions, 119.5px overflow, vs the slider path's 1 clip region that contains its
overflow).

_The crack in the hypothesis (why this is a reasoning problem, not a quick fix):_
`extents_unchanged` keys on the frame's bounds, and the trigger-drawer overflow exists only
while the frame is ALSO resizing (mid-tween), which changes the frame bounds → guard trips →
full self-clearing render → no ghost. So the guard MAY already prevent this exact ghost class.

_The one open reasoning question:_ is there any realistic in-place edit that keeps a card's
first-node (frame) bounds stable while changing what it paints outside that frame — OR that
paints into the never-repainted margins outside all sub-regions? If yes, the ghost is real;
fix = always repaint the inspector's opaque full-rect background before the dirty sub-regions
AND clip every card render to its own frame. If no, the guard covers the card case and the
culprit is the margins / a panel-boundary atlas-staleness issue (which fits the left-edge +
video-bleed fragments better).

_Repro difficulty:_ `render_ui_to_png` renders the tree directly and bypasses
`UICacheManager` entirely
([render.rs:44-51](../crates/manifold-app/src/ui_snapshot/render.rs#L44)), so NO existing
headless snapshot can show this class — every snapshot is a clean full render. A repro needs
a new harness driving `render_dirty_panels` across full→edit→incremental and reading back the
atlas. Blast radius is contained: only the inspector passes `sub_regions`; every other panel
full-renders when dirty and can't ghost this way. Handed to Fable as a reasoning task
(2026-07-07). Same family as BUG-025 (timeline-scissor-bleed).

**Verdict (Fable reasoning pass, 2026-07-07) — hypothesis REFUTED for the card-ghost class;
a different, real hole found; the video fragment exonerates the atlas entirely.**

_1. The card-ghost class cannot occur — three independent seals, verified in code:_
- **Every card-geometry tween runs under full invalidation, never the incremental path.**
  `tick_drawers` ([param_card.rs:1401](../crates/manifold-ui/src/panels/param_card.rs#L1401))
  bubbles collapse, spawn-pop, delete-fade, drawer-height AND tab-ink tweens into
  `drawer_anim_active`, which the app polls every frame
  ([app_render.rs:2940](../crates/manifold-app/src/app_render.rs#L2940)) → `needs_rebuild` →
  `invalidate_all()` ([:2842](../crates/manifold-app/src/app_render.rs#L2842)) → whole-atlas
  clear + full self-clearing renders. The trigger-drawer's unclipped ~120px overflow therefore
  never meets `LoadOp::Load` at all — the guard doesn't even need to catch it.
- **Bounds-stable-but-paints-outside edits don't exist in the card path.** Searched: the
  chevron's `Affine2::rotate` pivots about its own small rect (contained); slider fill/thumb/
  value-flash writes are contained under the card's opaque frame, which the incremental path
  always redraws first (`dirty_only=false`,
  [ui_cache_manager.rs:228](../crates/manifold-renderer/src/ui_cache_manager.rs#L228)).
- **The scroll-clip hole is already patched.** `traverse_flat_range` pre-pushes ancestor
  `CLIPS_CHILDREN` bounds for mid-tree ranges
  ([tree.rs:737-756](../crates/manifold-ui/src/tree.rs#L737)) — an incremental card repaint
  IS clipped by the scroll viewport. And the inspector's first node is a genuine full-rect
  opaque background ([inspector.rs:1892](../crates/manifold-ui/src/panels/inspector.rs#L1892)),
  so every full render self-clears the margins. The proposed fix direction ("repaint the
  background before dirty sub-regions") is actively WRONG: the background would overpaint the
  tab strip/chrome, which no sub-region would then redraw.

_2. The real hole (different from the hypothesis): out-of-sub-region dirt is silently
dropped._ The incremental path
([ui_cache_manager.rs:212-238](../crates/manifold-renderer/src/ui_cache_manager.rs#L212))
fires when ANY sub-region is dirty and repaints ONLY dirty sub-regions — it never checks for
dirt in the panel range that belongs to NO sub-region (tab strip, cog/Collapse controls,
scrollbar, all built directly in `build_in_rect`). `rendered_ranges` clears only the card
ranges, and the end-of-frame blanket `tree.clear_dirty()`
([app_render.rs:4807](../crates/manifold-app/src/app_render.rs#L4807)) then wipes the
remaining flags — erasing the evidence, so the fallback-to-full-render ("dirty list empty
next frame") never fires. The comment at
[app_render.rs:3870](../crates/manifold-app/src/app_render.rs#L3870) ("Deferred panels keep
their dirty flags") is falsified by :4807. **Trigger:** an in-place chrome mutation
co-occurring with card dirt — guaranteed whenever any param is audio-modulated (per-frame
card dirt), e.g. hover/unhover a tab or the Collapse button while a modulated generator
plays → the un-hover repaint is dropped and the stale hover state persists until the next
rebuild. This produces stale chrome STATES in place (ghost highlights, stale scrollbar) —
real, but probably NOT the screenshot's fragments. **Fix shape (root):** the incremental
path must detect dirt in the complement of the sub-regions and fall back to the full panel
render, and dirty-flag clearing for panel ranges should be owned by the cache manager (the
blanket `clear_dirty` may only touch the overlay region). **Sequencing: land BUG-060's clip
container first** — it bounds what this cache can be wrong about; order rationale in the
BUG-060 entry. This half is Opus-grade (fast-path regression risk), not Sonnet-mechanical.

_3. The video-bleed fragment cannot be atlas staleness at all._ The atlas never contains
compositor pixels: composite order is clear-to-black → atlas blit (pass 2,
[app_render.rs:3972](../crates/manifold-app/src/app_render.rs#L3972)) → compositor video
into `layout.video_area()` (pass 3, [:4001](../crates/manifold-app/src/app_render.rs#L4001),
opaque, aspect-fit INSIDE the rect) → timeline passes (4) → overlays (5, drawn straight into
the offscreen, [:4587](../crates/manifold-app/src/app_render.rs#L4587)). An atlas failure
shows black/transparent, never video. **Resolved by Peter 2026-07-07: the "video patch" in
the screenshot is just the preview window — legitimately there, not a bug.** (The composite
reasoning above stands for any future genuine video-over-UI sighting: it would implicate a
post-atlas pass, the BUG-025 class, never this cache.) The 2026-07-04 "sections interleaved"
sighting should be re-examined against hole #2 + rebuild-while-scrolled rather than the card
cache. The footer-overlap symptom this investigation started from is now its own entry with
a grounded root cause: **BUG-060** (no inspector-level pixel clip + the trigger-row drawer's
missing reveal clip).

**Outcome (Opus, 2026-07-08) — hole #2 (out-of-sub-region dirt dropped) FIXED at the root.**
BUG-060's clip container confirmed on `main` (@ `27557d18`) before starting, per the sequencing
note. Two-part structural fix:

1. _Incremental path now falls back on out-of-sub-region dirt._ New
`UITree::has_dirty_outside_ranges(start, end, covered)`
([tree.rs](../crates/manifold-ui/src/tree.rs)) reports DIRTY nodes in the panel range that lie
in no sub-region. The cache manager's incremental branch gates on a new `incremental_path_safe`
helper (`extents_unchanged` AND `!has_dirty_outside_ranges`)
([ui_cache_manager.rs](../crates/manifold-renderer/src/ui_cache_manager.rs)) — chrome dirt
(tab strip, cog/Collapse, scrollbar) now forces the full, self-clearing panel render the same
frame, so the stale-chrome ghost never paints. No one-frame lag: the fallback is same-frame,
not deferred.

2. _Panel-range dirty-flag clearing moved to the cache manager; blanket clear narrowed to the
overlay region._ The incremental path now returns the FULL panel range (safe: it only fires
when there's no out-of-sub-region dirt), so `clear_dirty_range` over `rendered_ranges`
([app_render.rs:3871](../crates/manifold-app/src/app_render.rs#L3871)) owns all panel-range
clearing. The end-of-frame blanket `clear_dirty()` at :4807 became
`clear_dirty_range(overlay_region_start, count)` — it no longer erases out-of-sub-region panel
dirt before the fallback can fire. The now-false comment at :3870 ("Deferred panels keep their
dirty flags") was corrected.

_Fast-path safety (the CRITICAL constraint):_ traced the tree layout — the 7 panels contiguously
tile `[0, overlay_region_start)` with zero gaps (build order: transport→header→footer→inspector
are back-to-back; the two split/resize handles are the SplitHandles catch-all
`[inspector_end, scroll_panels_start)`; layer_headers then viewport run to `overlay_region_start`;
`node_range() == (first, first+count)`). So clearing all rendered panel ranges + the overlay
range clears every node, exactly as the old blanket clear did — `has_dirty_in_range(0, panel_end)`
still settles to false and the `offscreen_dirty` idle fast path
([app_render.rs:3915](../crates/manifold-app/src/app_render.rs#L3915)) stays reachable. Both the
old and new end-of-frame clears run only on slow-path (dirty) frames — the fast path returns at
:3961 before either — so there is no clearing-parity change on idle frames. Since every dirty
panel is always rendered (no deferral in `render_dirty_panels`), no panel-range dirt can survive
a slow frame. Fast-path preservation is by reasoning + the tiling verification above, NOT a live
trace (the app is an interactive GPU rig I can't idle-observe headlessly here; `render_ui_to_png`
bypasses the cache).

_Verification:_ new device-free unit tests at the cache-manager helper layer —
`out_of_subregion_dirt_forces_full_render` (chrome dirt rejects the incremental path while
`extents_unchanged` still passes, isolating out-of-region dirt as the sole cause) and
`incremental_used_when_only_card_dirt` (in-card dirt stays on the fast path). Gate:
`cargo test -p manifold-renderer -p manifold-app -p manifold-ui` (993 + 158/10/1/3 + 646 passed)
+ `cargo clippy -p manifold-renderer -p manifold-app -p manifold-ui --all-targets -D warnings`
(clean; only pre-existing manifold-media Obj-C deprecation warnings). Shipped on
`fix/bug-015-out-of-region-dirt`. **Note:** this closes the stale-chrome-STATE class (ghost hover,
stale scrollbar). The 2026-07-04 "sections interleaved" sighting (hole #2 + rebuild-while-scrolled)
is a separate open thread if it recurs.

### BUG-060 — Inspector content paints over the footer bar — REOPENED 2026-07-08 (UI_CLIP_AND_Z P1 verified the wrong render path)
**Status:** REOPENED

**REOPENED 2026-07-08 (Peter, on latest main after P1 landed).** Still repros. New observations,
none yet explained:
- Inspector content bleeds below the panel's bottom edge into/over the footer (a stray param-slider
  fragment renders below the footer divider; see the report screenshots).
- **Swapping the inspector tab between Layer and Master clears it.** A full repaint makes it go away.
- It behaves *slightly* differently on generator inspectors (e.g. Plasma + stacked Color Compass)
  than on video-layer inspectors.

**Why the P1 "fix" didn't catch it:** P1's acceptance PNG (`bug060.after.png`) was rendered through
the headless snapshot harness, which draws via `UITree::traverse()` — the tier-sorted, region-clipped
path P1 actually changed. The **live app renders the main window via `panel_cache_info()` +
`UICacheManager`** (`crates/manifold-app/src/ui_root.rs`, `crates/manifold-renderer/src/ui_cache_manager.rs`),
which P1 did NOT change (it was flagged as VD-018). So the region clip was verified in a render path
the performer never sees, and the live path was never checked. The earlier "structural fix, dead by
construction" claim (below) was premature and applies only to `traverse()`.

**Cause: OPEN.** No confirmed root cause — do not assume one from the notes above; investigate the
live path directly. Next `BUG-NNN` for anything discovered en route.

**Investigation 2026-07-08 (Opus, 2nd pass) — tree-geometry cause ELIMINATED on the LIVE cache
path; cause localized below the UI tree, to the cache/dirty or offscreen layer.**

A new CPU test drives the *live* render path (`UIRoot::panel_cache_info` →
`UIRenderer::render_sub_region` → `UITree::traverse_flat_range`), not the headless `traverse()`
P1 checked. It builds the real `bug060` scene through `UIRoot::build()` (the D1 region wrap),
scrolls the inspector to the bottom with drawers open — 648 visible nodes, content raw-extending
to y=2870, **8 nodes straddling the footer edge (y=1180)** — and walks the inspector panel's node
range exactly as the cache manager does. Findings:
- **Zero** inspector nodes — rects OR text — paint below the footer top. The region clip is a
  fixed-bounds ancestor `traverse_flat_range` pre-pushes at *every* scroll offset and tween state
  ([tree.rs:962-977](../crates/manifold-ui/src/tree.rs#L962-L977)), so nothing can geometrically
  escape it. Text is CPU-clipped to `clip_bounds` ([native_text.rs:1121](../crates/manifold-renderer/src/native_text.rs#L1121)).
- The footer's OWN render on the cache path is correct: full-width background `(0,1180 1536×36)`,
  Q/FPS group intact, every node clipped to the full footer rect.
- Test: `crates/manifold-app/src/ui_snapshot/mod.rs` →
  `footer_leak_probe::cache_path_inspector_does_not_paint_below_footer_top` (feature `ui-snapshot`).
  This is the **footer-edge containment test P1 never had** — the P1 test
  (`layer_scroll_clip_prevents_scrolled_columns_painting_over_the_tab_strip`) checked the TOP edge
  (tab strip) via `traverse_range` + `panel.build`, i.e. neither the bottom edge nor the cache path.

So BOTH "inspector escapes its clip into the footer" (P1's claim) and "inspector content bleeds
below the panel" (this reopen's framing) are **disproven on the live path.** The bug is not a
mis-drawn or mis-clipped node. (The prior footer-leak session's own scissor trace agreed;
its "(B) untraced immediate-mode draws overwrite the footer" theory is **unsupported** — the
inspector/footer use no immediate-mode draws, that's the graph canvas only.)

Cache decision path traced (both layers), which explains the triggers:
- **Scroll:** `inspector.take_scrolled_in_place()` → `cm.invalidate_inspector()` *only*
  ([app_render.rs:960-963](../crates/manifold-app/src/app_render.rs#L960-L963)) — no atlas clear,
  footer's atlas region NOT repainted, frozen from the last clear.
- **Drawer expand:** `inspector.drawer_anim_active()` forces `needs_rebuild=true` every tween frame
  ([app_render.rs:2942-2944](../crates/manifold-app/src/app_render.rs#L2942)) → `build()` +
  `cm.invalidate_all()` → whole-atlas clear + all panels repaint (footer included); settles instantly.
- **Tab switch:** structural change → same `build()` + `invalidate_all()` → whole-atlas clear +
  footer repaint. **This is why swapping tabs clears the artifact** — it is a full recomposite.
- Second cache layer: the composited offscreen is re-blitted from cache unless `offscreen_dirty`
  ([app_render.rs:3951](../crates/manifold-app/src/app_render.rs#L3951)), set when any panel node in
  `[0, overlay_region_start)` is dirty ([:3122-3125](../crates/manifold-app/src/app_render.rs#L3122)).
  The atlas incremental sub-region path ([ui_cache_manager.rs:206-248](../crates/manifold-renderer/src/ui_cache_manager.rs#L206))
  repaints only dirty cards with `LoadOp::Load` and clears dirty for the whole inspector range — the
  same shape as **BUG-015** (stale pixels), fixed this week in this same file.

**Honest open gap:** by static reading, *no* write path puts wrong pixels into the footer band —
every UI draw clips at footer_top, and the footer repaints correctly on every clear. Yet the repro
persists and only a full clear fixes it, which is a *caching* signature (Peter's read, well-founded).

**CORRECTION (Peter, 2026-07-08) — the artifact is STALE UI CONTENT, not darkness.** The footer band
retains real UI pixels: UI colours, button / UI-chrome fragments left over from a prior render. It
is **not** a black or clear-colour gap. The prior footer-leak session's atlas dumps that read
"footer-right goes dark, RGB ~9-16" were a **harness failure** (bad atlas readback), not the live
symptom — do NOT cite them as evidence and do NOT chase a clear-colour / un-repainted-transparent
theory. Whatever lands in the footer band is old UI content that a full clear (tab-swap) wipes.

So the cause is a **stale-pixel / dirty-clear** failure the static read doesn't expose: UI content
that was validly drawn once is not cleared/repainted when it should be, so old chrome persists in
the footer band until the next whole-atlas clear. This is squarely the **BUG-015** class (the
incremental cache path leaving stale chrome), in the same file. Leading unverified suspects: (1) the
incremental sub-region path ([ui_cache_manager.rs:206-248](../crates/manifold-renderer/src/ui_cache_manager.rs#L206))
clears dirty for the whole inspector range while a node-index/extent shift leaves some band of
pixels un-repainted; (2) the footer's own prior-state pixels (old button positions/labels) surviving
a footer render that doesn't fully overpaint its rect; (3) another panel whose atlas rect or node
range overlaps the footer band leaving content there. Do not treat any as settled.

**Next step (needs LIVE pixels, not the geometry harness):** dump BOTH the atlas and the composited
offscreen footer band across the exact scroll + drawer + tab-swap repro, and identify *which* UI
content lands in the footer band and on *which* frame it is written-then-not-cleared — plus per-frame
`panel_valid` / `needs_clear` / `has_dirty_in_range(footer)`. That is the only oracle that shows the
stale content's source and the flag values at the instant it breaks. Prior instrumentation exists on
branch `fix/bug-060-footer-leak-trace` (worktree `.claude/worktrees/footer-leak`, env
`MANIFOLD_TRACE_FOOTER_LEAK=1`) but predates P1 AND its atlas readback is suspect (it produced the
bogus "dark" reading) — re-point it at current main and re-validate the readback first. The
`footer_leak_probe` cache-path containment test landed with this pass is the durable regression guard
for the geometry half (it proves the inspector does not geometrically leak, which stands).

**Symptom** (Peter, 2026-07-07; also the prior `f4b895d7` session's subject): with the
audio-mod drawer open on a Clip Trigger row (`is_trigger_gate`), scrolling the inspector to
the bottom paints card content over the footer bar.

**Root cause (two layers, both verified in code):**
1. **The inspector has no pixel clip of its own.** `build_in_rect` creates no
   `CLIPS_CHILDREN` container — the only `CLIPS_CHILDREN` reference in
   [inspector.rs](../crates/manifold-ui/src/panels/inspector.rs) is inside a test helper
   (:2711). Card visibility is managed by layout math, so any node that extends past the
   inspector's bottom edge paints straight into the footer's atlas region. The inspector
   renders AFTER the footer in the atlas panel loop
   ([ui_root.rs:480 `panel_cache_info`](../crates/manifold-app/src/ui_root.rs#L480) order),
   so its spill wins; the footer only repaints when IT dirties, so the spill persists.
2. **The concrete escape:** `build_toggle_trigger_row`'s drawer
   ([param_slider_shared.rs:1532](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1532))
   lacks the `drawer_reveal` clip `build_param_row` has (:2005-2017) — measured ~119.5px of
   unclipped paint below the card frame. A bottom-straddling card with that drawer open is
   exactly the repro.

**Fix (both landed):**
1. `build_in_rect` ([inspector.rs](../crates/manifold-ui/src/panels/inspector.rs)) now mints
   a `CLIPS_CHILDREN` node bounded to `(rect.x, columns_y, rect.width, columns_h)` before
   building either scroll column, and sweeps both columns' scroll clips (and everything
   built under them, `reparent_root_nodes`) under it once both are built — mirrors exactly
   how `ScrollContainer::reparent_content` already sweeps its own column. The pinned macros
   strip and tab strip chrome are built outside this range by construction, per the scope
   call in the original analysis.
2. `build_toggle_trigger_row` ([param_slider_shared.rs](../crates/manifold-ui/src/panels/param_slider_shared.rs))
   gained the same `drawer_reveal: Option<f32>` parameter and mid-tween `ClipRegion` wrap
   `build_param_row` already had — both call sites (effect and generator cards,
   [param_card.rs](../crates/manifold-ui/src/panels/param_card.rs)) now thread
   `self.drawer_height_anim` through it exactly like the slider-row call sites already did.
   `row_drawer_height`/`active_mod_tabs` already computed the correct tween target for
   `is_trigger_gate` rows (§9 folded them into the shared `ModTab::Audio` path) — this was
   pure wiring, not new height math.

**What's independently verified vs. what's inherited from the stated fix shape:** fix #2
(the mid-tween clip) is concretely proven by a new regression test,
`trigger_gate_drawer_tween_clips_midflight` (param_card.rs) — confirmed to fail on the
"builds under a clip region" assertion with the clip logic neutered and pass with it restored.
Fix #1 (the inspector-level container) was implemented exactly per the pre-decided scope and
verified to (a) not regress the `inspector` ui-snap scene (byte-identical render before/after)
and (b) pass the full `manifold-ui` lib suite (646/646) + `clippy -D warnings`. Digging into
*why* the footer-overpaint repro didn't reproduce via a plain headless render even pre-fix
turned up that `ScrollContainer::reparent_content` already sweeps card content under its own
column clip (bounded to the same inspector bottom edge) via `reparent_root_nodes` — so for
content built inside the master/layer scroll brackets specifically, the settled-state escape
this layer targets was already closed by existing infra; the new container is real
defense-in-depth for the "no pixel clip of its own" class as a whole (any future content built
directly in `build_in_rect` outside a scroll bracket), matching the pre-decided scope, but I
could not independently reproduce a settled-state footer overpaint this fix alone closes.
**BUG-071** (new, filed this session) documents a tooling gap that made this harder to verify
than it should've been.

**Verified:** `cargo test -p manifold-ui --lib` (646 passed) +
`cargo clippy -p manifold-ui -p manifold-app --all-targets -- -D warnings` (clean, aside from
pre-existing unrelated warnings). Shipped @ `27557d18` on `fix/bug-060-inspector-clip`.

**Order (Fable, 2026-07-07): fix BUG-060 FIRST, then BUG-015's stale-chrome hole.** The
clip container bounds what the atlas cache can ever be wrong about (no inspector pixel can
land outside its region afterwards), which shrinks the reasoning surface for the BUG-015
fix. BUG-060 is Sonnet-ready and verifiable with the existing headless snapshot tool (the
spill shows in plain full renders — before/after PNG of a bottom-straddling open trigger
drawer). BUG-015's hole is Opus-grade: it sits at the seam between the cache's incremental
path and the frame loop's blanket `clear_dirty`
([app_render.rs:4807](../crates/manifold-app/src/app_render.rs#L4807)), which exists
precisely because leftover dirty flags once defeated the idle fast path — a careless fix
reintroduces that regression; verify by reasoning + a unit test at the
`render_dirty_panels` helper layer (no snapshot can show it).

### BUG-018 — `node_graph::catalog_gen::tests::regenerates_in_sync` red on main: `docs/node_catalog.json` stale against the node registry — LOW
**Status:** OPEN

**Symptom** — found 2026-07-04, same full-workspace sweep as BUG-017, same shape: confirmed
pre-existing on origin/main (`90ab8531`) before the automation-P4 landing branch touched
anything — reproduced standalone in a disposable worktree at that exact commit.
`cargo test -p manifold-renderer --lib node_graph::catalog_gen::tests::regenerates_in_sync`
fails with `docs/node_catalog.json is stale`.

**Root cause** — not investigated; some session added/changed a node-graph primitive without
re-running `cargo run -p manifold-renderer --bin gen_node_catalog` afterward. Given `node_count`
sits at 214 in the checked-in file, worth diffing against the live-generated output to see
which node(s) are missing/changed before just overwriting.

**Fix shape** — mechanical: `cargo run -p manifold-renderer --bin gen_node_catalog`, commit
the regenerated `docs/node_catalog.json`. Same reasoning as BUG-017 for not fixing it this
session (unrelated to the work at hand, and worth doing once rather than mid-churn).

### BUG-019 — Motion "group fold" (D17) has no UI surface to fold — DESIGN GAP (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 completing UI motion P2. D17 lists "group fold: children
collapse into header," but the animation has nothing to animate: `EffectGroup.collapsed`
exists at the model layer (`crates/manifold-core/src/effects.rs:3194`) with zero rendering
surface — no group header, no collapse toggle, no child-card grouping by `group_id` in the
inspector (`rg EffectGroup crates/manifold-ui/src` → 0 hits).

**Root cause** — the design assumed a foldable effect-group UI in the inspector that was
never built. Group fold is a *new feature* (group header + child-card filtering + collapse
toggle), not an animation retrofit — correctly out of the motion layer's scope.

**Fix shape** — build the effect-group inspector UI first (own small design: header row,
`group_id`-keyed child filtering, collapse toggle), THEN the fold animation is a `FlipList`
+ exit-state retrofit like the other P2 collapses. Needs a design/build decision from Peter.

### BUG-020 — Card collapse animates effect cards but not generator cards — LOW (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 (UI motion P2 batch 1). Effect cards collapse/expand with the
`collapse_anim` reflow; generator cards do not — their rows parent at root (`None`) in
`ParamCardPanel::build_generator`, so there is no `ClipRegion` seam to clip the collapsing
body the way `build_effect` has.

**Fix shape** — give `build_generator` the same parent/clip-region seam `build_effect` uses,
then reuse the existing `collapse_anim`. Small, localized to `param_card.rs`.

### BUG-021 — Value snap-back is Perform-inspector only, not the graph-editor param cards — LOW (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 (UI motion P2 closer). Right-click value-reset eases the fill
(EASE_SNAP) on Perform-context inspector cards; the graph editor owns a separate
`ParamCardPanel` instance not reachable from the `ParamRightClick` dispatch site
(`ui_bridge/inspector.rs:1140`), so its value resets snap without the settle.

**Fix shape** — thread the snap-back trigger to the graph-editor's `ParamCardPanel` too, or
lift the reset-with-settle into shared `ParamCardPanel` logic both dispatch sites reach.

### BUG-025 — Timeline layer/header scissoring: clip content bleeds across row bounds — MED (repro needed)
**Status:** OPEN

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript) as "layer and
header scissoring": in the arrangement view, the bottom layer's purple clip body renders far
beyond its row — a solid block filling the timeline from its row down to the window edge —
while the layer-header column at bottom-left shows the Plasma MIDI drawer (MIDI / CHANNEL /
DEVICE) overlapping into that region. Clip content and header-column content are not being
mutually clipped to their rows/panes.

**Root cause** — unknown. Suspect surface: the per-row scissor rect for clip bodies (last or
expanded row), the `track-header-invariant` / `single-source-y-layout` class, or a stale
subregion scissor (`subregion-scissor-invariant`). Likely same family as BUG-015 (inspector
sections at stale offsets) — both smell like Y-layout/scissor divergence after the recent
timeline waves.

**Repro** — not pinned; NOT reproduced headless (2026-07-05 Opus). Snapshotted the `states`
and `timeline` scenes (both carry a selected generator layer with an open MIDI/CHANNEL/DEVICE
drawer, the closest fixtures to Peter's screenshot) — both render correctly: every clip body is
scissored to its row, every header drawer stays in the left column, group nesting clips fine.
A scroll-down + re-snapshot on `timeline` also did not reproduce (and scroll may not be fully
wired in the headless tracks path). So the general scissoring path is sound; the bug is
state-specific. Triage narrows it to a config the fixtures don't hit — most likely the
*last* row being a selected generator whose clip fills the remaining viewport height, and/or a
live scroll offset. Pin it with either a targeted fixture (selected generator as the final
layer) or a running-app repro from Peter's project.

**Repro attempt 2026-07-07 (timeline-ux audit)** — the 07-05 note's "scroll may not be fully
wired in the headless tracks path" is now explained: `--scroll` was seeded AFTER the base
render (fixed this branch), so every prior "scrolled" base PNG was actually unscrolled. With
scroll genuinely applied (via the interact after-render), headers + lanes offset together and
clip bodies stay scissored to their rows — still not reproduced. The state-specific triage
above stands.

**Fix shape** — TBD after repro. If it's the invariant class (likely, given BUG-015 is the same
family), fix at the single Y-layout source, not per-widget patches.

### BUG-026 — Batch-2 popups: entrance fade freezes at t=0 (transparent bg) until an input re-dirties the frame — MED — FIX LANDED, running-app verification owed
**Status:** OPEN

**Symptom** — reported by Peter 2026-07-05 (before/after screenshots): opening the Add Effect
browser renders the search field, filter chips, and preset cells floating directly over the
timeline — the popup's dark background panel is missing. Moving the mouse over the popup makes
the background appear and it then looks correct.

**Root cause (FOUND)** — not the alpha math, a missing animation-poll in the dirty-driven
renderer. The batch-2 popups (browser / ableton picker / settings) run a D17 entrance tween:
`enter_anim` starts at `t=0` and, while `t<0.999`, `BrowserPopupPanel::build` multiplies the
modal container's background + border alpha by `t` (browser_popup.rs:451,469-474) — so frame 0
draws the panel fully transparent while the cells (opaque, not `t`-gated) float on top. The
tween is ticked inside each popup's `update()`, which only re-runs while the frame stays dirty.
The inspector drawer + panel-split tweens self-sustain via a `needs_rebuild` poll after
`UIRoot::update()` (app_render.rs ~2927), but the batch-2 popups were added to `update()` and
never to that poll. Opening a popup dirties exactly one frame (drawing it invisible); nothing
re-dirties it, so the fade freezes at `t=0` until an unrelated input (mouseover) re-dirties the
frame — the "no background until mouseover" symptom.

**Fix (LANDED)** — added `is_animating()` to each batch-2 popup and the matching poll in the
app motion block, mirroring `drawer_anim_active` exactly. Gate: clippy `-D warnings` clean;
`manifold-ui --lib` 604/604. Commit `01c15213` (branch `fix/popup-enter-anim`).

**Verification owed (L4)** — the headless `--script` driver has no frame loop and its
`enter_anim` ticks off wall-clock, so it cannot exercise this timing bug; a running-app check
(open the Add Effect browser, confirm the background is present immediately without moving the
mouse) is the remaining proof. Tracked in VERIFICATION_DEBT (VD-006).

### BUG-031 — Layer context-menu + rename still address layers positionally — LOW (follow-up to the LayerId migration `877852a9`)
**Status:** OPEN

**Root cause** — the primary layer-header actions were migrated to carry a stable `LayerId`
(commit `877852a9`, kills the panel-index-vs-live-model collision). Two related clusters were
deliberately left positional to keep that diff bounded:
- The **`Context*Layer` right-click-menu family** (`ContextPasteAtLayer`, `ContextImportMidi`,
  `ContextAddVideoLayer/GeneratorLayer/AudioLayer`, `ContextDuplicateLayer`, `ContextUngroup`,
  `ContextDeleteLayer`, `DropdownContext::LayerContext`) still carry a `usize`. `LayerHeaderRightClicked`
  now carries the id and `ui_root` resolves it to the current row synchronously when the menu opens,
  so there's no regression — but the menu ITEMS bake in that index, leaving a (rare) stale window
  between menu-open and item-click.
- **`TextInputField::LayerName(usize)`** (layer rename): the enum derives `Copy`, and `LayerId`
  isn't `Copy`, so migrating it forces dropping `Copy` and cascades through the whole text-input
  subsystem (`app.rs` field handling). The double-click intercept resolves id→index locally, so the
  rename has the same (unchanged) stale window it always had.

**Symptom** — none observed; latent. A context-menu action or a rename committed after the layer
list changed under it (another command, undo/redo, MIDI phantom layer) could hit the wrong layer.
Same bug class as the migration killed for the primary controls.

**Fix shape** — carry `LayerId` in the `Context*Layer` family (thread it from
`LayerHeaderRightClicked` through the menu items) and switch `TextInputField::LayerName` to
`LayerId` (drop `Copy` from `TextInputField`, fix the fallout in `app.rs`). Mechanical, compiler-driven.

## Fixed

### BUG-062 (no-forward-version-guard) — an older build opening a newer .manifold silently strips unknown fields/effects and saves the loss back — HIGH (latent; becomes live the day two builds coexist)
**Status:** FIXED @ 1e349bf5

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P2.** A forward-version guard now runs at the top
of `load_project_from_json_with`, before migrate: the file's `projectVersion` is compared to
the single-source `CURRENT_PROJECT_VERSION` const, and a newer file is refused with
`LoadError::TooNew` — the Ableton-style message "This project was saved by a newer version of
MANIFOLD (project format X) than this build can open (Y). Update MANIFOLD to open it.",
surfaced through the existing load-error modal (no app change). A coarse secondary guard
refuses a newer archive `format_version`. No unknown-field round-trip (deferred by design D1 —
refusal is the honest fix while an old build can't render missing effects). See
docs/PROJECT_FILE_INTEGRITY_DESIGN.md.

**Found 2026-07-07 by the PROJECT_IO_MAP read (docs/PROJECT_IO_MAP.md §9 E1).**
`migrate_if_needed` (migrate.rs:5) only gates on `is_version_less_than` — there is no check
that the file's `projectVersion` is ≤ the build's ceiling (`Project::default` stamps
`"1.11.0"`, project.rs:1467). A newer file runs zero migrations, serde's
ignore-unknown-fields default drops every field the older binary doesn't know,
`strip_unknown_effects` (loader.rs:188) deletes newer effect types, and the next manual save
or 60s autosave writes the stripped project back — still carrying the newer version string,
so nothing ever notices. Scenario: laptop on last release opens the studio machine's current
show file once. **Fix shape:** before the typed deserialize, compare the file's
`projectVersion` against a build-version ceiling constant; refuse with a dialog (or open
read-only with autosave disabled). One constant + one comparison + one alert.

### BUG-064 (save-rename-before-fsync) — V2 save renames before fsyncing the temp file — MED (power-loss window replaces a good save with a torn one)
**Status:** FIXED @ 050e3fd7

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P1.** `save_v2_archive` now captures the `File`
returned by `zip.finish()` and calls `file.sync_all()` before the atomic rename (the existing
parent-directory fsync stays). Contents are durable *then* the durable rename points at them.
Verified at L1 (code inspection + a negative gate asserting two `sync_all` calls); power-loss
durability itself isn't unit-testable without fault injection — carried as a VERIFICATION_DEBT
line.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E3).** `save_v2_archive` (archive.rs:196)
writes the zip to a temp file, atomically renames it over the archive, then fsyncs the
parent directory — but never calls `sync_all()` on the temp file itself; `zip.finish()` only
flushes userspace buffers. On power loss the rename metadata can be durable while the file's
data blocks aren't: a correctly-named `.manifold` full of garbage that has already replaced
the previous good save (history blobs included — they live in the same zip). Venue power is
exactly the environment GIG_RESILIENCE_DESIGN plans for. **Fix shape:** one line —
`file.sync_all()` between `zip.finish()` and the rename (keep the File handle or reopen the
temp path).

### BUG-065 (24-bit-snapshot-hash) — save dedup and history identity key on 6 hex chars of SHA-256 — LOW probability / HIGH cost
**Status:** FIXED @ 050e3fd7

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P1.** `compute_hash` now returns 64 bits (16 hex
chars) instead of 24. Backward-compatible: old 6-char history entries keep their names and
copy forward untouched; the manifest's `current_hash` transitions on the next save; a
mixed-width archive's worst case is one *skipped* dedup (a redundant save), never a *wrong*
one.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E4).** `compute_hash` (archive.rs:289)
truncates SHA-256 to 24 bits for both the "no changes detected → skip save" dedup
(archive.rs:89) and `history/<hash>.json.gz` snapshot identity. A dedup collision silently
skips a real save; a history collision makes restore return the wrong snapshot. ~7×10⁻⁵ per
50-entry project lifetime — small, but the failure is silent data loss on the one
unrecoverable asset. **Fix shape:** widen to 16 hex chars (64 bits); entry names stay short,
old 6-char history entries stay readable (identity is string equality against the manifest,
so mixed-width archives keep working as saves roll over).

### BUG-061 (slider-reset-per-panel-lottery) — FIXED 2026-07-08 @ 480acf63 — right-click reset works on some sliders and not others; reset is per-panel hand-wiring instead of a slider behavior — MED (live recovery gesture a performer can't trust; reported by Peter 2026-07-07)
**Status:** FIXED @ 480acf63

**Fixed (root):** reset is now the slider's own gesture. Every slider carries a real default
(`BitmapSlider::build` stores it in `SliderNodeIds.default_normalized`; `SliderSpec.default`
threads through `ChromeHost`), and a single generic `PanelAction::SliderReset { snapshot,
changed, commit }` re-dispatches the slider's OWN value-change trio with the default baked
into `changed` — the exact Snapshot→Changed→Commit path a drag uses, so undo behaves like a
drag to that value (each `*Commit` already guards `old != new`, so resetting an at-default
slider is a no-op). One app-side handler recurses the trio (`ui_bridge/mod.rs`); the 7 bespoke
`*RightClick` actions and their duplicated handlers are gone. Covered: effect/generator params,
macros, master opacity, LED brightness, layer opacity, layer audio gain (new), modulation-drawer
sliders (new). Clip slip/loop sliders were already removed in `52920ab6` — deleted their dead
actions/handlers/stubs. Behavior change: a param reset now jumps (the eased snapback lost its
only caller), consistent with a drag. Guarded by per-surface tests that a right-click on the
track resolves to `SliderReset` with the declared default, so no panel can silently opt out
again. Excluded surfaces → BUG-070. Original diagnosis retained below.

**Symptom:** right-click-to-reset-to-default works on effect/generator param sliders, macros,
master opacity, LED brightness, and layer opacity — and silently does nothing on clip
slip/loop sliders, the modulation drawer sliders, and the Audio Setup gain sliders. On stage
this is the recovery gesture (crank a param too far, snap it home in one click); a gesture
that only works on some faders is one you can't use without thinking.

**Root cause (structural, investigated 2026-07-07):** reset is not a slider behavior — it's a
per-panel contract wired twice: each panel registers a bespoke `PanelAction` on the track node
in `register_intents` (e.g. `ParamRightClick` at `param_card.rs:3707`, `MacroRightClick` at
`macros_panel.rs:466`, `MasterOpacityRightClick`/`LedBrightnessRightClick` at
`master_chrome.rs:365-368`, `LayerOpacityRightClick` at `layer_chrome.rs:267`), and
`ui_bridge/inspector.rs` handles each one separately, re-deriving the default. Any panel that
skips or loses either half silently has no reset. Two proofs it's the wiring model:

- **Clip slip/loop is a regression.** Reset shipped in `b78dc9ba` (March), then `52920ab6`
  deleted clip_chrome's legacy event handler during the intent-registry migration and the
  right-click was never re-registered. The app-side handlers still sit dead at
  `ui_bridge/inspector.rs:1017` (`ClipSlipRightClick`) and `:1032` (`ClipLoopRightClick`) —
  no UI code emits those actions anymore.
- **The infra has a vestigial field.** `SliderNodeIds.default_normalized` (`slider.rs:47`),
  commented "for right-click reset", is written once with the *initial* value (not the
  default) and read by nothing — reset was meant to live in the widget and never did.

Drawer sliders (`drawer.rs:331`) and Audio Setup gain sliders never had reset at all. The
graph editor's inline param sliders use right-click for the mapping popover instead —
intentional, but decide explicitly whether that surface keeps the divergent gesture.

**Fix shape (root):** make reset the slider's own gesture. Every slider already has a
value-change path (Snapshot → Changed → Commit, same one a drag uses); give
`BitmapSlider`/`SliderSpec`/`SliderController` a real `default` value and have right-click on
any track synthesize "set to default" through that existing path. Every slider gets reset by
construction, the bespoke `*RightClick` actions and their duplicated app-side handlers
collapse into the generic path, and the clip slip/loop regression fixes itself. New sliders
can't opt out by forgetting. Watch the two non-uniform cases: param-card sliders whose
right-click currently carries `(target, param_id, default)` context, and label/row right-click
menus (`ParamLabelRightClick` etc.) which are context menus, not reset — they stay.

### BUG-078 (generator-runtime-reshapes-from-stale-meta-params) — a structural-rebuild's reshape read the graph's stale `preset_metadata.params` shadow because the constructor took no `ParamManifest` — LOW — FIXED 2026-07-09 on `fix/bug-078-reshape-manifest` (confirmed by regression test, then fixed same session)
**Status:** FIXED (2026-07-09)

**Symptom:** calibrate a generator param (widen its range / add a curve) → make
a structural graph edit (add/remove a node) before saving → the *rendered* param
mapping could momentarily revert to the pre-calibration reshape. Bounded and
non-data-loss: the authoritative `PresetInstance.params[id].spec` (the manifest)
was never touched, and the correct reshape reasserted itself the moment the
project was saved and reloaded (D12 derives `meta.params` from the manifest at
serialize time).

**Root cause:** `PresetRuntime::from_def` (`crates/manifold-renderer/src/preset_runtime.rs`)
built its `param_reshape: AHashMap<String, (min, max, curve, invert)>` — the
map every generator binding's [`Reshape`](crates/manifold-renderer/src/node_graph/param_binding.rs:273)
is resolved from at construction — entirely from `doc.preset_metadata.params`,
the shadow. `from_def` / `from_def_with_device` / `from_json_str_with_device`
took no `ParamManifest` parameter, so no code path could hand a live,
post-calibration manifest spec to the reshape. This was the generator analog of
the effect path's `synth_user_binding` (`manifold-core/src/effects.rs:1752-1783`),
which already reads the manifest (`self.params.get(&b.id)`) post-P2; generators
never got the equivalent wiring because their whole binding list (stock +
user-added) resolves through one shared `doc.preset_metadata` path.

**Fix (shipped):** threaded `Option<&ParamManifest>` through the constructor
chain — `from_def` / `from_def_with_device` / `from_json_str_with_device` →
`GeneratorRegistry::create` / `create_with_override` →
`GeneratorRenderer::install_layer_generator` / `acquire_clip`. When the manifest
is present, `from_def` overlays each param's reshape (min/max/curve/invert) from
the manifest `spec` over the shadow, manifest-wins-per-id; when `None` it keeps
reading the shadow (correct for a fresh-from-disk standalone build). The one live
caller — `generator_renderer.rs`'s `start_clip` and the per-frame `render_all`
structural-rebuild sweep — passes `layer.gen_params().params`. Every other caller
(thumbnails, `check_presets`, `freeze_profile`, gltf import, freeze proofs, the
cold-start thumbnail path, type-swap rebuild) passes `None` and is byte-identical.
`from_json_str` (mock/test) keeps its 2-arg signature, passing `None` internally.
The empty-fast-path and the `preset_metadata.bindings` scale/offset +
`string_bindings` reads were left untouched.

**Regression test** (now green, in the default suite):
`crates/manifold-renderer/src/preset_runtime.rs`,
`generator_runtime_tests::generator_rebuild_reshape_honors_live_manifest_over_stale_shadow`.
A def whose `preset_metadata.params` `amt` spec is fixed at
`min=0,max=1,curve=Exponential` (the stale shadow) is rebuilt via
`PresetRuntime::from_def` with `Some(&values)` where `values` carries the
recalibrated `min=0,max=2` — the reshape now resolves value 1.0 to `0.5` (the
fresh 0..2 range), where the pre-fix output was `1.0` (the stale 0..1 range).
The bug is only observable when the reshape has a non-identity curve/invert
(`apply_card_reshape` only consults min/max when `invert || curve != Linear`).

**Escaped:** `wave/param-boundaries-p2` (`254792c0`) — dual-write deletion
(D4) landed correctly for the manifest side but left this generator-only
construction-time read pointed at the now-unmaintained shadow · caught-by:
review (audit during PARAM_STORAGE_BOUNDARIES P2, not the wave's own gate —
no existing test exercised a structural rebuild after a calibration).

### BUG-077 (test-fixtures-not-region-wrapped) — 17 tests across `manifold-renderer` + `manifold-ui` mint root-parented nodes outside a region and panic on the D4 ownership assertion — LOW (pre-existing, found 2026-07-09 during PARAM_STORAGE_BOUNDARIES P3's full-workspace sweep) — FIXED 2026-07-09 (`fix/bug-077-uicache-regions`); workspace fully D4-clean
**Status:** FIXED (2026-07-09)

**Symptom:** `cargo test --workspace` fails 17 tests, all with the same panic:

```
thread '...' panicked at crates/manifold-ui/src/tree.rs:290:9:
root-parented node minted outside an open UITree::begin_region — UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md D1/D4.
Wrap this subtree's build in begin_region(...)/end_region(...) instead of rooting it at the tree.
```

The full failing set (the whole D4-conformance class — enumerated iteratively via
`--no-fail-fast` runs, each fix surfacing the next binary the previous fail-fast had
hidden):

- **6 in `manifold-renderer/src/ui_cache_manager.rs` unit tests:**
  `extent_change_forces_fallback`, `extents_unchanged_when_bounds_stable`,
  `incremental_used_when_only_card_dirt`, `no_subregions_signature_is_empty`,
  `out_of_subregion_dirt_forces_full_render`, `partition_change_forces_fallback`
  (surfaced by the `ui_cache_manager` test filter — found first).
- **6 in `manifold-renderer/tests/ui_color_swatches.rs` snapshot tests:**
  `header_demo`, `footer_demo`, `transport_demo`, `modulation_drawer_sheet`,
  `browser_popup_demo`, `browser_popup_thumbnails_paint` (surface only on a full
  crate run, not the narrow `ui_cache_manager` filter).
- **5 in `manifold-ui/tests/chrome_param_card_proof.rs`:**
  `badge_toggle_is_in_place`, `build_matches_card_structure`,
  `intents_resolve_and_fold_up`, `opening_drawer_needs_rebuild_then_grows`,
  `value_change_reconciles_in_place` (surface only on the full-workspace run — a
  different crate; the 6th test in the file, `validate_catches_unwired_control`,
  builds no tree and always passed).

**Root cause:** `0bb51dad` ("region mechanism — ZTier, RegionToken,
begin_region/end_region, D4 enforcement") landed the D4 root-parented-node panic
guard (`mint`'s `debug_assert!` at `tree.rs:290`, `#[cfg(not(test))]` — so it is
active for any *non-`manifold-ui`* dependent, which every one of these test
binaries is: `manifold-renderer`'s own tests, and `manifold-ui`'s *integration*
tests, which compile `manifold-ui` as an external non-test dependency). These test
fixtures still build their tree directly against the root, outside any
`begin_region`/`end_region` pair — they were never migrated to the region contract.
One bug class, three files, two crates.

**Confirmed unrelated to PARAM_STORAGE_BOUNDARIES P3:** `git diff --stat` for the
P3 session touches only `crates/manifold-io/src/migrations/param_storage_v14.rs`;
`manifold-ui`/`manifold-renderer` are untouched. The `ui_color_swatches` half was
additionally confirmed pre-existing by `git stash`ing the `ui_cache_manager` fix
and rerunning `--test ui_color_swatches` against the base commit `b15e5c20` — the
same 6 fail identically, so no half is caused or masked by another.

**Escaped:** `wave/param-boundaries-p1` or an earlier UI-region wave (whichever
landed `0bb51dad` without touching these fixtures) · caught-by: the next phase's
full-workspace sweep (P3) plus this fix session's own crate-wide and workspace gate
runs, not that wave's own gate — the region-enforcement landing's test scope did not
include a full `cargo test --workspace` run.

**Fixed** — test files only; no production code touched, `tree.rs:290`'s D4
assertion unchanged. Every failing fixture now wraps its tree build in a single
`tree.begin_region(rect, tier, label, UIFlags::empty())` / `tree.end_region(region,
start)` bracket, matching the idiom real callers use (`ui_root.rs`'s per-panel
pairs; the closer precedent for these flat, non-tiered fixtures is
`ui_snapshot/render.rs`'s single-region wrap and the `split_handles` region — both
use a no-op-clip rect precisely so the region's `CLIPS_CHILDREN` is a guaranteed
no-op and the rendered pixels are unchanged). Proof of completeness:
`cargo test --workspace --no-fail-fast 2>&1 | rg 'tree.rs:290'` returns **zero**
hits — the whole D4 class is gone on this branch.

- `crates/manifold-renderer/src/ui_cache_manager.rs`: the two fixture builders
  (`tree_with_subregions`, `tree_with_chrome_and_card`) now open one region around
  their whole build. Wrapping shifts every node index by the region container node
  `begin_region` mints first, so the fixtures return the panel's own `node_start`
  (`tree.count()` captured right after `begin_region`) and compute sub-region ranges
  relative to it; the six tests reference edges via that `start` (or `subs[i].0`,
  `end - 1`) instead of the old absolute literals.
- `crates/manifold-renderer/tests/ui_color_swatches.rs`: the six snapshot tests wrap
  their `panel.build(...)` / `popup.build(...)` / slider+drawer build loop in a
  full-canvas region (Chrome tier for the transport/header/footer panels, Overlay
  for the browser popups, Base for the mod-drawer sheet — semantically faithful,
  but with one region per test the tier only labels intent). Note that since
  `0bb51dad` `render_tree` walks *registered regions* (`traverse` → `traverse_regions`),
  not root-parented nodes, so the wrap is also what makes each test's panel/popup
  tree content render at all — an unwrapped build registers no region for the
  traversal to visit. Verified by rerunning the suite: all six now produce their
  PNGs and pass.
- `crates/manifold-ui/tests/chrome_param_card_proof.rs`: the shared `ProofCard::build`
  helper (every failing test routes through it) opens one region around the
  `ChromeHost::build` call, region rect == the card rect. Safe because the region
  container is minted directly on the tree, NOT through the host — so `ChromeHost`'s
  own `ids`/`node_count`/DFS indices (which the tests assert on exactly:
  `host.node_id(N)`, `host.node_count()`, `t.count()`) are untouched; the host bases
  off `tree.count()` at build start, exactly as its own
  `build_assigns_contiguous_ids_from_tail` unit test already proves for a mid-tree
  build. The intent fold-up tests still resolve correctly: `IntentRegistry::resolve`
  stops at the first ancestor carrying the gesture/area-claim (always a host node
  below the region), and the region node carries neither, so the extra transparent
  ancestor changes nothing.

### BUG-075 (timeline-drag-end-never-finalizes) — the terminal DragEnd for trim/marquee/move was dropped, so on_end_drag never ran — HIGH — FIXED 2026-07-08 (found + fixed same session)
**Status:** FIXED (2026-07-08)

**Symptom:** after the DRAG_CAPTURE landing (P1 `6e4bddcb`), timeline
trim-drag and click-drag marquee/region-select never finalized on release —
the trim didn't commit, the selection box stayed live, `drag_mode` stuck.
Clip *move* looked fine but silently lost its undo snapshot and left the
same stuck `drag_mode` (its live per-frame preview masked the miss).

**Root cause:** ordering bug in `ui_root::process_events`. The terminal
`DragEnd`/`PointerUp` arm called `broadcast_gesture_end()`, which set
`drag_owner = None`, BEFORE the post-match `should_stash_for_tracks(event)`
read `drag_owner` to decide whether to stash the event for
`InteractionOverlay`. With the owner already nulled, the terminal `DragEnd`
was never pushed to `viewport_events`, and `on_end_drag` (the sole finalizer,
`app_render.rs`) never ran. Confirmed empirically by an adversarial repro
driving the real `process_events` path: stashed kinds = PointerDown,
DragBegin, Drag — no DragEnd. The existing ownership unit tests set
`drag_owner` by hand and called `should_stash_for_tracks` directly, bypassing
the broadcast-before-stash seam, which is why it shipped.

**Fix:** split `broadcast_gesture_end` into `fire_gesture_end_hooks()`
(overlay hooks only) + the fused clear. The terminal arm fires the hooks but
defers the `drag_owner = None` clear to the end of the iteration, after the
stash read; the PointerDown self-heal keeps the fused `broadcast_gesture_end`
so a lost-OS-release still clears a stale owner. Guarded by a new
`process_events`-driven regression test
(`timeline_drag_end_reaches_viewport_events_through_process_events`) that
drives Down→Move→Up and asserts the terminal DragEnd reaches
`drain_viewport_events()`.

### BUG-052 (sample-rate-dependent-detection) — onset + kick detection mis-tunes at non-48k sample rates — FIXED 2026-07-07 @ 6e0e8988
**Status:** FIXED @ 6e0e8988

**Fixed:** `SpectrogramConfig::with_time_grid_for(sample_rate)` (manifold-spectral) rescales
`hop`/`n_fft` from the 48k reference so a hop is always ~5.33 ms and the window ~85 ms; the
analyzer applies it at build (`analysis.rs` `StreamingSendAnalyzer::new`). Frequency bins were
already SR-invariant, so nothing there changed. No-op at 48k, exact 2× at 96k. Proven by
`time_grid_holds_hop_and_window_duration_across_rates` across 44.1/48/88.2/96/192k plus the full
manifold-audio analysis suite (46 tests green). **Still owed (VD, cheap):** the end-to-end proof
named in the original gate — resample a fixture to 96k, run the harness, confirm fire TIMES in
seconds match the 48k run. The grid-invariance test makes this belt-and-suspenders, not load-bearing.
Original diagnosis retained below.



**Found 2026-07-07 (Peter's question during the kick-detector discussion).** The audio
analysis runs at the DEVICE'S native rate — `audio_mod_runtime.rs:322` sets the analyzer
rate to `device_rate`, and the resampler only aligns layer audio to that rate, never to a
canonical one. Every timing constant is in HOPS (`ODF_MEDIAN_HOPS`, `ONSET_REFRACTORY_HOPS`,
`KICK_WIN`/`KICK_AGE_CAP`) and every rate constant is bins-per-hop (`KICK_STEP_MAX`, the D5
tracker slew), while a hop is `256/sample_rate` seconds. At 96k a hop is 2.7 ms (half of
48k's 5.3 ms), so the kick detector's "14 bins within a 10-hop window" spans only 27 ms and
the kick's ~90 ms chirp has descended only ~10 bins by then — under the 14-bin threshold, so
**the kick detector goes near-deaf at 96k** (the whole onset analysis mis-tunes, though the
adaptive-threshold/refractory parts degrade more gently). Bins are already SR-invariant
(log-freq CQT anchored at 10 Hz), so `drop_bins` needs no change. Confirmed by arithmetic +
the device-rate code path; NOT observed on a 96k run (Peter: no need to prove in code).

**Fix shape (root, Peter-directed):** normalize the analysis TIME GRID, not the sample rate
(no resampling) — derive `hop` and `n_fft` in samples from the device SR so a hop is always
~5.3 ms and the window ~85 ms (`SR/n_fft` stays 11.7 Hz, so frequency resolution is
unchanged). Then every hop-count and bins-per-hop constant is literally unchanged and
automatically invariant; no fixed ODF ring becomes a Vec. Cost: larger FFT at higher rates
(proportional to the extra data). Rejected alternative: keep hop=256 samples and scale all
hop-COUNTS with SR — more blast radius (dynamic ring sizes) for no gain. Gate: resample a
fixture to 96k, run the harness, confirm fire TIMES in seconds match the 48k run (the eval
already grades in seconds). Fold into the kick time-constant rework.

### BUG-058 (drag-end-consumable) — timeline stuck in move/trim mode: DragEnd is a routable/consumable event, but N independent drag-state owners depend on receiving it — HIGH (live editing gesture wedges; reported by Peter 2026-07-07) — FIXED 2026-07-08 (DRAG_CAPTURE P1–P3)
**Status:** FIXED (2026-07-08)

**Fixed 2026-07-08 — DRAG_CAPTURE P1–P3.** The root fix shipped: single drag-capture ownership (P1 `6e4bddcb`, D1–D4) makes drag-terminal events non-consumable broadcasts routed to the owner by identity, eliminating the eater class (open dropdown/modal can no longer swallow the terminal DragEnd). P2 `12683746` z-aware window seams; P3 `2fc4cfbd` per-widget immediate-drag threshold. The P1 landing also exposed and fixed BUG-075 (dropped terminal DragEnd). Original report retained below.

**Symptom** — sometimes a clip move/trim doesn't release: after the mouse leaves the
timeline (typically onto the inspector) and the button is released, the timeline stays in
move/trim mode (cursor stuck as Move/ResizeHorizontal, next interaction behaves as if the
drag were still live). Self-heals on the next clip press (`on_begin_drag` overwrites
`drag_mode`), which is why it reads as intermittent.

**Root cause (architecture)** — a drag's terminal event must reach every drag-state owner,
but the routing treats it as consumable, first-match-wins. `InteractionOverlay.drag_mode`
(EXCLUSIVE owner of clip move/trim/region state, `interaction_overlay.rs`) is only cleared
by `on_end_drag`, which only runs if the `DragEnd` survives `process_events`' routing
gauntlet (`ui_root.rs:1411` overlay-first) and reaches the tracks-area stash
(`ui_root.rs:1445`). Confirmed eaters:
- `dropdown.rs:695-702` — an open dropdown consumes ALL `DragBegin`/`Drag`/`DragEnd`
  unconditionally (e.g. an accidental two-finger right-click mid-drag opens the clip context
  menu; the release's DragEnd is then eaten by the menu).
- any `Modality::Modal` overlay open at release captures everything, even events it ignores
  (`ui_root.rs:1003-1006`).
The input layer already learned this lesson twice: `859bbceb` made `DragEnd`/`PointerUp`
unconditional at the `UIInputSystem` level, and `process_events` routes
`DragEnd|PointerUp` to the inspector + layer_headers in a dedicated UNCONDITIONAL second
loop (`ui_root.rs:1499-1515`) — but the timeline overlay's DragEnd still travels the
consumable path plus a positional gate (`is_event_in_tracks_area`) patched by a boolean
latch (`overlay_drag_active`). Also unverified at the OS seam: winit-macOS delivery of
`MouseInput(Released)` when the release lands outside the window (inspector is at the right
edge; BUG-028 precedent says winit macOS seams are real). Cheap decisive oracle if the
eater-list explanation doesn't hold: eprintln at four seams (Up received in
`primary_mouse_input` / DragEnd emitted in `input.rs` / stashed in `process_events` /
`on_end_drag` entered), reproduce once, read which link broke.

**Instrumentation SHIPPED 2026-07-07** (`feat/drag-capture-instrumentation`): launch with
`MANIFOLD_INPUT_TRACE=1` and every discrete pointer transition prints a `[input-trace]`
line at each seam — window interceptors, input-system press/release/drag-begin, overlay
routing (which overlay consumed/captured), tracks-area stash gate (with latch state), and
timeline-overlay begin/end (with drag mode). One repro of the stuck state names the broken
link. Root fix is `docs/DRAG_CAPTURE_DESIGN.md`.

**Fix shape (root)** — make drag-terminal events non-consumable broadcasts: every
drag-state owner (InteractionOverlay, inspector, layer_headers, every overlay panel with an
armed drag) receives `DragEnd`/`PointerUp` regardless of routing outcome; overlays may
still *act* on it but never block it. Cleaner still: single drag-capture ownership — at
`DragBegin` one owner is recorded, all subsequent `Drag`/`DragEnd` route to that owner by
identity (not position), killing the latch, the positional gate, and the eater class in one
move.

### BUG-059 (band-line-grab-falls-through) — Audio Setup band-divider grabs are sticky, and a MISSED grab silently drags clips/region under the modal — HIGH (silent project edits during calibration; reported by Peter 2026-07-07) — FIXED 2026-07-08 (DRAG_CAPTURE P1–P3)
**Status:** FIXED (2026-07-08)

**Fixed 2026-07-08 — DRAG_CAPTURE P1–P3.** The silent-edit fall-through (the HIGH) was first closed 2026-07-07 by the `swallow_drag` origin-claim stopgap, then superseded by P2 `12683746` z-aware window seams; P3 `2fc4cfbd` added the per-widget immediate-drag threshold for the divider lines (the 4px dead-zone). Root: DRAG_CAPTURE single-owner capture. Original report retained below.

**Symptom** — the horizontal crossover lines in the Audio Setup spectrogram are hard to
grab: fine adjustments stick, grabs sometimes do nothing, and (unreported but confirmed in
code) a missed grab over the timeline area starts an invisible clip move / region select
UNDERNEATH the modal, committing real project edits.

**Root cause (several, stacked)** —
- **Missed-grab fall-through (the HIGH):** the panel's `PointerDown` arm returns `Ignored`
  when the press misses a divider/label (`audio_setup_panel.rs:2269-2294`), and the panel is
  `Modality::Modeless`, so the whole DragBegin/Drag/DragEnd family falls through to the
  layers beneath. The tracks-area stash gate classifies by RAW POSITION with zero z-order
  awareness (`ui_root.rs:2455` `is_event_in_tracks_area`), so a drag starting on the modal
  background over the timeline is stashed and `InteractionOverlay` hit-tests clips by
  position (clips aren't tree nodes) — editing the project through the modal. The `Click`
  arm was already patched to swallow exactly this (`owns_node(id) || point_in_scope(*pos)`,
  the prior fix attempt Peter remembers); the drag family wasn't.
- **4px dead zone:** the global `DRAG_THRESHOLD_PX = 4.0` (`color.rs:837`) applies to a
  precision control — no `Drag` event fires for the first 4px, so sub-4px nudges are
  impossible and every grab starts sticky.
- **Window-seam interceptors punch through the modal:** `primary_mouse_input` checks
  `is_near_split_handle` (6px full-width band at the timeline's top edge) and
  `is_near_inspector_edge` (±4px at `insp.x`, full window height) BEFORE overlay routing and
  BEFORE hit-testing (`window_input.rs:274-310`) — when the centered modal overlaps those
  zones, a press on a band line there is stolen for a panel-resize drag.
- **Dropdown-dismiss swallow:** with any dropdown open (the panel has device/layer
  dropdowns), the next press outside it is consumed by the dismiss branch
  (`window_input.rs:269-273`) — first grab after touching a dropdown always dies.
- **Scope-dark deadness:** `scope_fmin <= 0` (no capture yet) makes dividers ungrabbable by
  design (`audio_setup_panel.rs:422`) — reads as "sometimes it just doesn't work" if lines
  are visible before audio flows.
- **First-click-dead — TRACE-CONFIRMED 2026-07-07 (Peter, `MANIFOLD_INPUT_TRACE=1`), the
  dominant "always the second click works" mechanism:** the press arms the band drag and is
  consumed by the panel, but by threshold-crossing the pressed node no longer resolves
  (`DRAG-BEGIN … resolves=false` in the trace; dead and working clicks carried different
  WidgetIds) — and `input.rs` `process_pointer` emits `DragBegin` and every `Drag` ONLY
  while the pressed widget resolves, so the entire motion stream is silently swallowed and
  the armed position-based drag never hears a move. Same disease `859bbceb` fixed for the
  terminal events, unfixed for motion. Probable node-death path (inferred): the panel's own
  consume sets `overlay_dirty` → overlay rebuild between Down and threshold. Root fix:
  `DRAG_CAPTURE_DESIGN.md` D9 (added same day) — unconditional `DragBegin`/`Drag` emission
  with `node_id: Option<NodeId>`, in P1.

**Fix shape** — same root as BUG-058's capture model plus locals: (1) the modeless panel's
`PointerDown`/drag family must swallow anything inside the panel rect (mirror the `Click`
arm) — that alone kills the silent-edit hole; (2) per-widget drag threshold (0 for the
divider lines — arm on press, track raw moves); (3) window-seam interceptors must respect
z-order (both handles already have tree nodes — route them through hit-testing instead of
raw-position pre-checks); (4) hover-glow the grab zone only when actually grabbable
(scope live).

**(1) SHIPPED 2026-07-07 as an explicit stopgap** (`feat/drag-capture-instrumentation`,
superseded by `docs/DRAG_CAPTURE_DESIGN.md`): the panel now claims the whole
DragBegin/Drag/DragEnd family for any drag whose ORIGIN is inside `panel_rect`
(`swallow_drag`), keyed on origin so a timeline drag crossing the panel still passes
through; unit tests `missed_grab_drag_inside_panel_is_swallowed` +
`timeline_drag_crossing_panel_passes_through`. The silent-edit hole is closed; the feel
items (2)–(4) remain for the design.

### BUG-055 (eval-harness-stale-time-grid) — both audio eval harnesses used the unscaled default hop on non-48k files — FIXED 2026-07-07 (kick P5 retune branch)
**Status:** FIXED (2026-07-07)

**Symptom:** kick exact-match gate drifted ±1–5 fires per 44.1 kHz drums fixture; mod_harness
CSV `time_s`, PNG bar grid, and printed hop line stretched 8.8% on 44.1k files. **Root cause:**
BUG-052 made the LIVE analyzer's time grid rate-invariant (`with_time_grid_for`), but neither
example harness followed: `hpss_proto::build_clip` hopped 256 native samples, and `mod_harness`
built its own unscaled `SpectrogramConfig` for its feed cadence and time base — so it pushed
256-sample chunks against a 235-sample analyzer hop and sampled `latest()` at the wrong rate,
silently missing/duplicating fires in every per-hop record on 44.1k input. The P2/P4 kick
"exact match" was measured through this sampler. **Fix:** both harnesses now scale their config
(`with_time_grid_for`); `StreamingSendAnalyzer::hop()` accessor added so consumers can't
re-derive it stale; `mod_harness` `debug_assert`s its grid equals the analyzer's. Residual
documented in KICK_SWEEP_EVENT_DESIGN §P5: offline replay vs live stream legitimately diverge
for ridges born during the fade-in (window-fill) region only.

### BUG-036 (dead-LFO-on-reload) — LFO on an imported-glb generator's card param is dead after project reload; re-importing the same .glb revives it — MED — FIXED 2026-07-06
**Status:** FIXED (2026-07-06)

**FIXED 2026-07-06** — both halves of the fix shape below, plus two siblings the audit
found in the same class:
- **Ordering (root):** `manifold_io::loader` gained `_with` variants that hand the file's
  `embeddedPresets` to an installer BEFORE the typed `Project` deserialize
  ([loader.rs](../crates/manifold-io/src/loader.rs) `EmbeddedPresetsPrePass`); the app
  passes `install_embedded_presets` so the overlay + core registry are populated when the
  V1.4 param loader resolves each instance ([project_io.rs](../crates/manifold-app/src/project_io.rs)).
- **Keep-don't-drop (class-kill):** `build_param_manifest` now only drops an unknown id
  when the template actually RESOLVED and says the id is gone (informed deprecation).
  With no template at all, the entry is kept on a placeholder spec — state is never lost
  to a missing template ([effects.rs](../crates/manifold-core/src/effects.rs)).
- **Sibling 1:** history-snapshot restore/open-copy never installed the snapshot's
  overlay at all (params dropped AND stale overlay left live) — now go through
  `load_project_snapshot_with` + an unconditional overlay install at the
  `apply_project_io_action` seam.
- **Sibling 2:** New Project never cleared the previous project's overlay (fork leak) —
  covered by the same apply-seam install.
Verified against the real repro: `meshImportTests.manifold` loads with all 17 imported
card params present and the saved `cam_orbit` driver resolving; regression test
`crates/manifold-app/tests/project_local_preset_reload.rs` proves both defenses
independently.

**Symptom** (Peter, 2026-07-06, `~/Downloads/meshImportTests.manifold`) — a project saved
with a glb auto-built graph (the `assemble_import_graph` door) reloads fine visually, but
an LFO bound to one of its card params (Camera Orbit) doesn't run. Deleting the layer and
re-creating it by dropping the SAME .glb makes the identical LFO run. So the modulation
path works against a freshly-imported instance and not against the deserialized one.

**Root cause — SMOKING GUN in the 2026-07-06 trace-run log.** On project load, EVERY card
param of the imported preset is dropped at deserialization:
`[manifold-core] dropping unknown param id "cam_orbit" on PresetTypeId(cc0_japanese_apricot_prunus_mume#2) load (no template descriptor, no inline spec)`
— same for cam_dist/cam_fov/cam_tilt, sun_int/x/y/z, metal_0..3, rough_0..3, env_bright.
The LFO is inert because its target param no longer exists in the loaded manifest. The
drop lines appear BEFORE `[presets] merging 4 project generator preset(s)` in the log:
the V1.4 param loader resolves specs against the template registry, and project-local
(imported) preset templates are merged into the registry only AFTER the project's layer
data deserializes — so every param keyed to a project-local preset type resolves to "no
template descriptor" and is dropped. Re-importing works because a fresh import registers
the template first. Almost certainly a param-storage-redesign (landed 2026-07-05)
load-ordering regression, cousin of the known-RED `expose_mirror` test.

**Fix shape** — order the loader so project-local preset templates register before layer
param deserialization; AND (class-kill, per `eliminate-bug-class-at-storage-layer`)
make the loader keep an unresolvable param as an inline spec instead of dropping it —
silent data loss on load is the storage-layer bug class this repo already decided to
eliminate. The drop log line should become a hard test assertion (load the repro project,
assert zero drops).

**Repro** — load `meshImportTests.manifold`, press play: Camera Orbit LFO inert. Delete
layer, drag the .glb back in, rebind: runs.

### BUG-029 — `profiling` feature doesn't compile: rotted against the Beats/Bpm newtypes — FIXED 2026-07-06
**Status:** FIXED (2026-07-06)

**Fix** — the three newtype casts (`.as_f32()` / `.0`) applied; `cargo check -p manifold-app
--features profiling` and clippy are clean, default build untouched. Un-parked because the
profiler is the next oracle for BUG-035 (per-frame content-thread phase breakdown, LFO on vs
off). Toggling the perf HUD starts/stops a session when built with `--features profiling`
(input_host.rs `toggle_performance_hud`); sessions land in `profiling_sessions/`. Note: GPU
pass-level numbers are still zero on native Metal (pre-migration profiler) — the CPU phase
breakdown (engine tick / render_content / gpu_poll) is the usable signal.

**Root cause** — the `#[cfg(feature = "profiling")]` blocks in `manifold-app` predate the
`Beats`/`Bpm`/`Seconds` newtype migration and still treat those values as raw `f32`/`u32`.
Three sites: [content_thread.rs:854](../crates/manifold-app/src/content_thread.rs#L854)
(`Beats as u32` — non-primitive cast), [content_thread.rs:988](../crates/manifold-app/src/content_thread.rs#L988)
(`expected f32, found Beats`), and [content_commands.rs:933](../crates/manifold-app/src/content_commands.rs#L933)
(`expected f32, found Bpm`).

**Symptom** — `cargo build -p manifold-app --features profiling` fails with 3 `E0308`/`E0605`
type errors. The default build (profiling off) is unaffected, which is why the rot went
unnoticed — the feature evidently hasn't been compiled since the newtype migration landed.

**Found during** — PARAM_STORAGE P2 (2026-07-05), while compile-checking the profiling path
after migrating its param readout from the deleted positional `param_values` to `ParamManifest`
(that param-side migration is done and correct; these 3 errors are unrelated newtype-cast rot
in the same blocks).

**Fix shape** — wrap each site in the Beats/Bpm accessor instead of a raw cast (~3 one-line
fixes). Unrelated to param storage, so parked here rather than folded into P2.

### BUG-033 — `ui-snapshot` feature build broken: `manifold_core::effects::resolve_param_in` no longer exists — FIXED (verified in-tree 2026-07-07)
**Status:** FIXED (2026-07-07)

**Fixed note (2026-07-07, timeline-ux pass)** — `lane_param_range` now reads
`param.spec.min/max` directly (interact.rs:497), the broken `resolve_param_in` call is gone,
and the harness builds AND runs on the 2026-07-07 tip (`cargo build -p manifold-app --features
ui-snapshot` clean; all scenes + `--script` flows rendered this session). Fixed by a landing
between 07-05 and 07-07 that didn't close this entry; closing on direct evidence.

**Root cause** — [interact.rs:500](../crates/manifold-app/src/ui_snapshot/interact.rs#L500) (`lane_param_range`, an
automation-lane interact verb) calls `manifold_core::effects::resolve_param_in(&def, fx, param_id)`
to read a param's `(min, max)`. That function/module path is gone after the PARAM_STORAGE
refactor (the range now lives on the `ParamManifest`/spec, not a `resolve_param_in` helper).

**Symptom** — `cargo build --bin manifold --features ui-snapshot` fails with `E0425` (unknown
function) + a knock-on `E0433`. The DEFAULT build is unaffected, so it went unnoticed — but it
means the entire `ui-snap` headless harness (graph/editor/timeline PNG + `--script` driver) can't
compile on trunk. Found 2026-07-05 (Opus) while rendering a BUG-027 verification PNG; worked
around with a temporary local stub (reverted) to get the render.

**Fix shape** — resolve the param spec through the current manifest API and read its min/max
(mirror whatever `lane_param_range`'s live-app equivalent now does). Owner: PARAM_STORAGE P2 (its
refactor moved the range); ~1 site. Unrelated to the LayerId / node-preview work in this session.

### BUG-013 — `commit_and_wait_completed` never checks command-buffer status (likely the GPU-proof flake mechanism) — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Root cause** — [encoder.rs:1655-1662](../crates/manifold-gpu/src/metal/encoder.rs#L1655-L1662):
`waitUntilCompleted()` returns on ANY terminal state including `Error`; no caller checks
`status()`/`error()`. Every heavy freeze proof and `TextureDiff::compare` submit through this
call and read the result back as if it succeeded. Under cross-binary GPU contention
(documented in `.config/nextest.toml` and the `GPU_TEST_LOCK` comment; three call sites build
unlocked devices), a transiently failed buffer reads back stale/partial → spurious large diff.

**Status** — split verdict, judged REAL-as-flake-mechanism: it precisely explains the
observed signature (several heavy tests, random divergence sizes, never reproducing
isolated). It is test-infra, not a compiler miscompile — but it gates trust in the entire
oracle suite, so it blocks using the suite as a hard gate for agent work.

**Fix shape** — check the buffer's terminal status in `commit_and_wait_completed`; on error,
panic in tests (fail loudly, retryable) and log in production. Then re-baseline the flake:
if red runs now report command-buffer errors instead of pixel diffs, the mechanism is
confirmed; if divergences persist with clean status, keep hunting.

**FIXED 2026-07-05** — [encoder.rs](../crates/manifold-gpu/src/metal/encoder.rs) now calls a
`verify_completed()` helper after `waitUntilCompleted()`: if the buffer's status isn't
`Completed`, it reads `status`/`error()` and, in `debug_assertions` builds (tests + dev),
panics with the code+message; in release (the live show) it logs and continues rather than
crash mid-set. The dev-vs-release split via `cfg!(debug_assertions)` gives "loud in tests,
survivable on stage" without a test-only cfg (the helper lives in `manifold-gpu`, whose tests
aren't where the flake showed up). The `GPU_TEST_LOCK` "three unlocked sites" note above was
partly stale: the lock is a `parking_lot` reentrant mutex inside `test_device()`, and every
lib GPU test acquires it; the only unlocked device is the `gpu_proofs` integration binary's
own `GpuDevice::new()`, which runs in a separate process. That cross-process contention is now
self-reporting (a contended failure panics instead of reading stale pixels) rather than
silent, so a dedicated cross-process lock is no longer needed. Landed alongside the GPU-test
`gpu-proofs` feature gate (default `cargo test` is now GPU-free; run `--features gpu-proofs`
to exercise the proofs).

### BUG-016 — Imported .glb layers are black boxes: no card params, no Model File picker, edit paths silently no-op — FIXED 2026-07-04 (`2d5e4dc6`)
**Status:** FIXED (2026-07-04)

**Resolution** — PRESET_LIBRARY P0 (D9) shipped: the drop now registers the assembled
graph as a project-embedded preset (`origin: Saved`) and the layer TRACKS it (`graph:
None`); the assembler emits a curated 13-slider card (camera/sun/envmap/per-object
material) with real bindings; the app installs the catalog overlay before the layer is
created, so the process-global preset registry seeds `init_defaults` consistently on both
threads. The `graph_def_mut` override install is deleted. verify-at-impl #4 resolved
(`bundled_preset_json` reads the overlay-merged catalog, no change needed). Assembler +
command tests + GPU render proofs green. **Still owed: the live drag-drop manual gate** in
a running app (card sliders move pixels, editor opens on the cog, save/reload intact) — the
one thing only Peter can eyeball. Original analysis below for reference.

**Root cause** — the glTF Stage-4 install mints a preset id that resolves in no catalog and
stashes the def only on the layer
([app_lifecycle.rs:506](../crates/manifold-app/src/app_lifecycle.rs#L506),
[layer.rs:100](../crates/manifold-editing/src/commands/layer.rs#L100)). Every type-keyed
surface then fails independently: the assembler emits empty `params`/`bindings`
([gltf_import.rs](../crates/manifold-renderer/src/node_graph/gltf_import.rs), metadata block)
so the card is empty; generator string params are sourced from the registry only
([inspector.rs:2251](../crates/manifold-app/src/ui_bridge/inspector.rs#L2251)) so the Model
File picker never shows; the editor's catalog default is `None`, which gates several edit
dispatch arms into silent no-ops (e.g. [app.rs:1356](../crates/manifold-app/src/app.rs#L1356)).
The reported empty editor canvas is NOT fully root-caused: `GraphSnapshot::from_def` on the
assembled def is proven good (12 nodes / 10 wires), so the entry path loses the watch target —
observe at repro.

**Fix shape** — `PRESET_LIBRARY_DESIGN.md` P0 (D9): the drop registers an `EmbeddedPreset`
and the layer tracks it; assembler emits curated performance bindings. Not per-consumer
fallbacks.

### BUG-017 — `docs_index_is_in_sync_with_docs_dir` red on main: two design docs never regenerated the index — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Symptom** — found 2026-07-04 running the full workspace sweep for the automation-P4
landing (unrelated to that work — pre-existing on origin/main before the landing branch
touched anything, confirmed via `git show 90ab8531:docs/README.md`).
`cargo test -p manifold-core --test docs_index_sync` fails:
`docs/README.md is out of sync with docs/. Missing from the index: ["AUDIO_SENDS_UX_DESIGN.md",
"TIMELINE_INGEST_DESIGN.md"]`.

**Root cause** — two sessions added design docs (`AUDIO_SENDS_UX_DESIGN.md`,
`TIMELINE_INGEST_DESIGN.md`) without re-running the generator afterward.

**Fix shape** — mechanical: `python3 scripts/gen_docs_index.py`, commit the regenerated
`docs/README.md`. Not fixed this session because other sessions were actively adding more
docs concurrently — regenerating now risked going stale again within the hour. Whichever
session next touches `docs/` and finds the tree quiet should run the generator and close
this out.

**Fixed 2026-07-05** — regenerated while adding `VERIFICATION_DEBT.md` (orchestration-quality
pass); `cargo test -p manifold-core --test docs_index_sync` green, 103 docs indexed.

### BUG-022 — Main-window browser popup: Escape while the search field is focused cancels the text session but leaves the popup open — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Resolution** — applied the documented fix shape: in the main-window `text_input.active` Escape arm
(`window_input.rs`), when `field == SearchFilter`, also call
`self.ws.ui_root.browser_popup.handle_escape()` alongside `text_input.cancel()`, mirroring the
editor window's node-picker branch — one press now dismisses both the search field and the popup.
The closed-overlay pump reconciles the already-cancelled session next frame. Compiles + clippy clean.
Owed: the in-app one-press-closes confirmation (headless can't drive it), but the code mirrors the
proven editor branch exactly. Original analysis below.

**Symptom** — found 2026-07-04 auditing `window_input.rs`'s keyboard routing while
implementing `docs/OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`. For the MAIN window (effect/
generator browser), once the search field has focus (`self.text_input.active &&
field == SearchFilter`), every keystroke is intercepted by the `if self.text_input.active { ... }`
block in `window_input.rs` (`primary_keyboard_input`, ~line 1593) before it ever reaches
`UIRoot::process_events`/`route_overlay_event`. Its `Key::Named(NamedKey::Escape)` arm calls
only `self.text_input.cancel()` — it never touches `self.ws.ui_root.browser_popup`. So Escape
while typing clears the search text and ends the text session, but the popup itself stays
open; a second Escape (now routed normally, since `text_input.active` is false) is needed to
actually dismiss it. This is plausibly the exact mechanism behind Peter's original report
("the search and text seems to stay after you search and need to click elsewhere again to
close it properly") — P1's stash-and-drain fix (`TextSessionOwner`/`take_closed_overlays`)
closes the *orphaned-session-after-popup-closes-elsewhere* class, but this is the inverse:
popup not closing when the session ends.

Note the EDITOR window's analogous bespoke branch (`window_input.rs` ~1145, node picker) does
NOT have this gap — its Escape arm already calls `browser_popup.handle_escape()` directly
alongside cancelling the text input (now also wired through `note_overlay_closed_if` as part
of this session's P1 work).

**Root cause** — the main-window `text_input.active` Escape arm was written before the browser
popup existed as an `Overlay`-driven modal; it only ever needed to cancel a plain text field.
Nothing updated it when `BrowserPopupPanel` started hosting a `SearchFilter` session.

**Fix shape** — in the main-window Escape arm, when `self.text_input.field == SearchFilter`,
also call `self.ws.ui_root.browser_popup.handle_escape()` (mirroring the editor's branch) instead
of only `self.text_input.cancel()`. Small, localized to `window_input.rs`'s
`if self.text_input.active` block — no design-doc scope change, since this is a pre-existing
gap outside P1/P2's stated deliverables (which target orphaned-session-on-close, not
missing-close-on-cancel).

### BUG-024 — Generator preset thumbnails render on a WHITE background (unrepresentative) — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Resolution** — root cause was (a) from the suspect list: generators leave their background
transparent (alpha 0), and `readback_tonemapped_rgba8` saved that alpha into the PNG, so viewers
showed the transparent background as white. Fixed by compositing over opaque black in the readback
(`rgb * a`, force alpha 255) — generators produce straight (non-premultiplied) alpha per
[[alpha-standardisation]], so `rgb * a` is the correct over-black composite, and opaque content
(effects, a=1) is byte-identical. Verified by regenerating + Reading the PNGs: StarField now reads
as stars on black, Lissajous as a clean curve on black, Bloom (effect) unchanged and correct.
**Residual (separate, minor):** a few full-frame generators still read low-saturation in their bare
state — Plasma is a grey blob on black (its background is now correct, but its bare/default output
without audio modulation or a colormap param is desaturated). Not the white-bg bug; a per-generator
"bare look" issue, low priority — leave for a thumbnail-polish pass if it matters on the picker.

### BUG-024-ORIG — original analysis (Generator thumbnails on WHITE background) — superseded by the FIXED note above
**Status:** SUPERSEDED

**Symptom** — found 2026-07-05 eyeballing the committed `assets/preset-thumbnails/generators/*.png`
after adding warm-up frames (PRESET_LIBRARY P6). Effect thumbnails (rendered over the gradient
fixture) look correct (Bloom reads right). But GENERATOR thumbnails render their content over a
WHITE background instead of the generator's own (usually dark) field: StarField is dark specks on
white (should be bright stars on black); Plasma is a grey blob on white. Warm-up frames (t advances,
state accumulates) did NOT fix it — so this is a render-path issue, not cold-start.

**Root cause** — unknown, not yet diagnosed. Suspects in
`crates/manifold-renderer/src/preset_thumbnail.rs::render_generator`: (a) the `Rgba16Float` render
target isn't cleared to the generator's expected background (black/transparent) before
`runtime.render`, so unwritten/low-alpha regions read as white after `readback_tonemapped_rgba8`;
(b) premultiplied-alpha / straight-alpha mismatch in the readback vs how generators composite
(cf. [[alpha-standardisation]] — compositor is premultiplied, producers aren't); (c) the tonemap
maps the clear/HDR default toward white. The live `GeneratorRenderer` path composites over the
correct background, so comparing its clear/blend setup against this one-shot path should localize it.

**Fix shape** — likely: clear the thumbnail target to the same background the live generator path
uses (black or transparent) before rendering, and match its alpha convention in the readback. Then
regenerate the 46 factory PNGs via `cargo run -p manifold-renderer --bin generate-preset-thumbnails`.
Effects are unaffected. Until fixed, generator thumbnails are present but not visually usable — the
P6 image-cell display infra is correct; the generator render output is not.

### BUG-023 — `no_new_raw_color_literals` red on main: real count (201) one above baseline (200) — FIXED 2026-07-05 (in the P6 landing)
**Status:** FIXED (2026-07-05)

**Resolution** — the extra raw literal was localized (not a "prior session" — it was THIS
orchestration's own P5 landing `0d6e857e`): `browser_popup.rs` carried
`const BADGE_TEXT: Color32 = Color32::new(130, 130, 134, 255)` for the origin-badge text,
added by P5 and missed because that phase ran clippy + focused tests but not the
`design_tokens` integration guard. Fixed by tokenizing it into `color::BROWSER_CELL_BADGE_TEXT`
(color.rs is the scan's exempt token home), dropping the counted set back to 200. Guard green.
Lesson for the orchestration: run `-p manifold-ui --test design_tokens` on any phase that
adds UI color, not just clippy. Original analysis below.

**Symptom** — found 2026-07-05 running the full gate for `PRESET_LIBRARY_DESIGN.md` P6
(thumbnails). `cargo test -p manifold-ui --test design_tokens no_new_raw_color_literals` fails:
`Raw Color32::new( count rose to 201 (baseline 200)`. Confirmed pre-existing and unrelated to
P6: re-ran the same scan logic against `git show HEAD:<path>` for every file under
`crates/manifold-ui/src` (a standalone Python re-implementation of `scan()`/`classify()`) and got
201 on HEAD alone, before any P6 edit — the P6 changes to `browser_popup.rs`/`color.rs` net to
**zero** new raw literals (three new cells' worth of `Color32::new(` were added to `color.rs`,
which the scan excludes as the token home, and the matching local consts in `browser_popup.rs`
were pointed at those new tokens instead of a raw literal — no net change to the counted set).

**Root cause** — not investigated; some prior session's commit added exactly one raw
`Color32::new(` line somewhere under `crates/manifold-ui/src` without bumping
`COLOR_BASELINE` in `crates/manifold-ui/tests/design_tokens.rs` (or without using a
`// design-token-exempt:` comment for a genuine one-off). `git bisect`/`git log -S"Color32::new("`
over the file list the scan touches would localize it quickly; not run this session since it's
orthogonal to P6 and risked burning session budget chasing an unrelated one-line drift.

**Fix shape** — mechanical, one of: (a) find the extra raw literal and tokenize it (count back to
200, no baseline change), or (b) if it's a genuine one-off, add `// design-token-exempt: <reason>`
on that line (count back to 200), or (c) bump `COLOR_BASELINE` to 201 if it's accepted debt. Not
fixed this session — the gate confirms the diff at hand is P6-clean; picking apart an unrelated
pre-existing count belongs to whoever next touches `manifold-ui/src`'s colour call sites.

### BUG-027 — Graph-editor node previews composite on the wrong z-layer vs. node chrome — MED — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Fix** — node previews now draw INLINE via a new `Painter::draw_image_uv` primitive, emitted by
`GraphCanvas::draw_node` right after each node's body, with each node pushed to its OWN increasing
depth band (`CONTENT+1+i`); the renderer's per-depth loop draws that band's rects then its image,
and a node stacked above (higher band) occludes a lower node's preview. Both flat post-pass blits
(live `app_render.rs`, headless `ui_snapshot/render.rs`) are deleted; the live path registers the
rotating atlas front via `UIRenderer::register_external_texture` + a per-cell UV, the harness
registers each node's output texture. Verified: a deterministic depth-band unit test
(`node_previews_render_in_per_node_depth_bands`) proves the occlusion ordering, and a Kaleidoscope
effect-graph PNG confirms real previews render inline correctly. Full default suite green.

---
_Original analysis (kept for the record):_

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript): node preview
thumbnails overlap neighbouring nodes inconsistently — a preview (e.g. Luma to Color) draws
OVER another node's body/ports while that node's own chrome draws over the preview, so
stacking order disagrees within a single node pair. Previews look like they live on a
separate layer that ignores node z-order.

**Root cause** — KNOWN (2026-07-05 Opus, deeper read; the earlier "unknown" was wrong). The
node preview thumbnails are NOT part of the depth-ordered chrome render at all — they're a
SEPARATE flat blit pass issued AFTER the whole chrome is composited, in `visible_node_thumbnails`
order (no depth). Both paths do it identically:
- Live app: [app_render.rs](../crates/manifold-app/src/app_render.rs) clears the offscreen to the
  canvas bg (a `clear`, not a drawn rect), renders chrome + black preview-screen placeholders via
  the depth-ordered tree/canvas pass, presents to the drawable, then blits each node's atlas cell
  over the drawable in a final flat loop (~L3668).
- Headless harness: [ui_snapshot/render.rs](../crates/manifold-app/src/ui_snapshot/render.rs)
  `render_graph_to_png` does the same — chrome first, then a `ui-snap-graph-thumbs` blit loop over
  each node's output texture (~L228).
Because every thumbnail is painted after every node body, no node body can occlude a preview, and
a lower node's preview lands over a higher node's body. The reason it's a bolt-on post-pass: the
immediate-mode `Painter` trait (`draw.rs`) has rect/line/text primitives but **no textured-quad
primitive**, so previews couldn't be drawn inline with the node bodies and were blitted separately.

**Repro** — IS headless-reachable (the earlier entry said it wasn't — wrong). `render_graph_to_png`
reproduces the exact flat-blit bug; render two overlapping preview-emitting nodes and the lower
node's thumbnail draws over the higher node's body. That gives a before/after PNG to verify a fix.

**Fix shape** — depth-interleave the previews instead of post-blitting them: add a thumbnail-draw
primitive to the `Painter` trait, have `canvas.render` emit each node's preview inline right after
its body (so occlusion follows node draw order), route it through the existing depth-interleaved
Image pipeline in `ui_renderer.rs` (which already draws per-depth: rects, then images, then text —
needs the rotating node atlas bound + a per-cell UV subrect for the live path; the harness feeds
per-node output textures with full UV), and delete BOTH flat blit passes. Real immediate-mode
renderer change (Painter trait + UIRenderer + canvas render + both blit-pass deletions), but
headless-verifiable. Not a "patch the overlap cases" job.

### BUG-028 — File-drop targeting can't read the live pointer during a Finder drag (both AppKit poll sources frozen) — MED — FIXED 2026-07-05 (`wave/timeline-drop`, landed on main 2026-07-05; Peter's live-drag verification still owed)
**Status:** FIXED (2026-07-05)

**Symptom** — dragging an audio file onto an existing audio lane lands it on a NEW lane
instead of the target lane. Verified 2026-07-05 (Peter, live drag test).

**Root cause** — the `DroppedFile` arms in `app.rs` resolve their target from `cursor_pos`,
which winit freezes for the whole drag (its macOS backend implements no `draggingUpdated:`
and emits no `CursorMoved` during a drag session). Both AppKit poll fallbacks were live-tested
and are ALSO frozen during an NSDragging session: `mouseLocationOutsideOfEventStream` and
`+[NSEvent mouseLocation]` both returned byte-identical values across dozens of frames while
the pointer was actively moving. The poll site (`about_to_wait`) runs during the drag, so the
loop isn't starved — the position APIs simply don't update while macOS owns the drag. Polling
is a dead end.

**Fix (as built)** — `crates/manifold-app/src/drag_interpose.rs`: winit's macOS drag
destination is its `NSWindow`'s window delegate (not a view), and that delegate implements
`draggingEntered:`/`performDragOperation:`/etc. but NOT `draggingUpdated:`. At startup we
`class_addMethod` a fresh `draggingUpdated:` onto the delegate's class (returns
`NSDragOperationCopy`) and swizzle the existing `performDragOperation:` (so the drop position
is captured even if the pointer never moves again after entry), both stashing
`[sender draggingLocation]` — converted window-point → view-point (`convertPoint:fromView:nil`)
→ flipped to `cursor_pos`'s logical top-left convention — into a UI-thread-only cell. New
`crates/manifold-app/src/drag_hover.rs` (`DragHoverTracker`) wraps it; all three `DroppedFile`
arms (audio/MIDI, image, glTF) in `app.rs` now read
`drag_tracker.drop_position().unwrap_or(cursor_pos)`. P2 (drop-target ghost): a full-length
translucent preview clip renders on the target audio lane during the drag
(`app_render.rs`, reusing the existing `ClipBody`/`emit_clips`/ghost-alpha pipeline that
in-app clip-move drags already use); the "New lane: ⟨filename⟩" label and a discrete beat-line
for the non-audio-lane case were **not** built — no existing floating-text-over-viewport
primitive to reuse, out of scope for this pass. Overrides TIMELINE_INGEST_DESIGN §2 D1 (see
its §3 for the full poll-failure writeup, now superseded).

**Verification** — clean compile + clippy (`-D warnings`) + full `manifold-app` test suite,
plus 4 new unit tests for the coordinate flip (`drag_interpose::macos::tests`). The one thing
that can't be verified headless: whether `NSWindow` actually forwards `draggingUpdated:` to a
delegate that only gained the method at runtime (documented AppKit behavior, `respondsToSelector:`
is checked per-message — but only a live drag proves it). Gate: drag a Finder audio file over an
existing audio lane → joins that lane at the pointer's beat, ghost clip shows lane+length before
drop; an image drop lands under the pointer.

### BUG-032 — glTF import: a model with >2 materials fails to load ("unknown parameter 'pos_x_2'") and renders black — HIGH — FIXED 2026-07-05 (`dc97bbe6`)
**Status:** FIXED (2026-07-05)

> Id note: originally logged as BUG-029 (commit `dc97bbe6`, commit-message and
> the `prove-render-path` memory still say 029). A concurrent PARAM_STORAGE P2
> session independently used BUG-029 for the profiling-compile bug (still Open,
> above) and added BUG-030. To resolve the collision without splitting that
> open sequential pair, this closed entry was renumbered to BUG-032. The
> `dc97bbe6` commit reference is immutable history — this entry is canonical.

**Symptom** — Peter, 2026-07-05: importing `cc0__japanese_apricot_prunus_mume.glb` (4 distinct
materials) produced a black viewport and a repeating log flood: `Generator … failed to load from
def: graph load error: node 4 (node.render_scene): unknown parameter 'pos_x_2'` +
`Generator type … not found in the preset catalog`. Escaped: glTF wave / PRESET_LIBRARY P0 ·
caught-by: **held-out input in the running app** (the VD-003 mesh-snapshot render harness looked
green because it exercises `gltf::import` directly, NOT the production `PresetRuntime::from_def`
load path where the failure lives — a wrong-path verification, see VERIFICATION_DEBT VD-003).

**Root cause** — `node.render_scene` is the first primitive whose PARAM set (not just its ports)
grows with a reconfigure param: per-object transforms `pos_x_N`/`pos_y_N`/… exist only after the
node reconfigures to `objects >= N+1`. The def loader (`graph_loader::instantiate_def`)
snapshotted the declared param surface ONCE at the node's default 2-object count, then validated
every def param against that stale snapshot — so `pos_x_2` (object index 2, present for the
apricot's 4 objects) was rejected as unknown before the node ever reconfigured. The runtime calls
`node.reconfigure(&params)` after every build (graph.rs, snapshot.rs, freeze/region.rs); the
loader was the one path that didn't. mux_texture/multi_blend hid the gap because their reconfigure
grows PORTS (validated at wire time), not params; the azalea dev fixture hid it because it has
exactly 2 objects.

**Fix** — call `boxed.reconfigure(&doc_params)` before the `param_defs` snapshot in the loader
(mirrors snapshot.rs: seed declared defaults, override with doc values, reconfigure). No-op for
static-shape nodes; general across every reconfigure-param node. Verified on the REAL path: the
apricot `.glb` (4 objects) now loads clean through `PresetRuntime::from_def`. Regression tests:
`render_scene_with_three_objects_loads_per_object_transform_params` (synthetic, portable) +
`held_out_gltf_generator_loads_through_from_def` (`#[ignore]`, env-gated on a >2-material `.glb`).

### BUG-040 (v13-import-migration-drop) — V1.3→V1.4 migration drops positional params of a project-local (imported) generator — LOW (narrow window) — FIXED 2026-07-09 (`wave/param-boundaries-p3`, PARAM_STORAGE_BOUNDARIES_DESIGN.md P3)
**Status:** FIXED (2026-07-09)

**Found** during the 2026-07-06 param-system post-refactor audit (BUG-036 sibling hunt),
by reading `crates/manifold-io/src/migrations/param_storage_v14.rs` — not reproduced on a
real file.

**Mechanism** — the migration maps positional `paramValues` to ids via (a) the instance's
own `graph.presetMetadata.params` order, else (b) the baked `LEGACY_PARAM_ORDER` table.
A TRACKING instance of an imported/forked generator has `graph: None` and its type id is
project-local, so it's absent from the baked table → arm (b) drops the values with the
"not in the baked LEGACY_PARAM_ORDER" warning and the instance loads with template
defaults. The file itself carries the missing order: `embeddedPresets[type].def
.presetMetadata.params`.

**Exposure** — only projects saved between the glTF import door landing (2026-07-04) and
the V1.4 wire landing (2026-07-05) can hold positional params for a project-local type;
anything saved since writes the id-keyed map. The drop is loud (warning), one-time, and
values-only (defaults still load).

**Fix shape** — in `param_storage_v14`, between the per-instance-graph arm and the baked
table, consult the project tree's own `embeddedPresets` for the type's
`def.presetMetadata.params` order (pure `Value → Value`, self-contained in the same
file). Unit fixture: positional generator instance + matching embedded preset.

**Fixed** — `positional_ids` (`crates/manifold-io/src/migrations/param_storage_v14.rs`)
gained a new arm ("case 1.5") between the own-graph arm and the WireframeDepth/baked-table
arms: a generator with no own `graph.presetMetadata.params` now consults
`embedded_param_orders(root)` — a lookup keyed by each `embeddedPresets[i].def
.presetMetadata.id`, built once (read-only) before the mutable per-instance walk — before
falling through to `LEGACY_PARAM_ORDER`. Three tests cover it:
`bug040_positional_generator_with_matching_embedded_preset_resolves_by_its_order` (the fix),
`bug040_positional_generator_without_matching_embedded_preset_falls_to_baked_table` (unchanged
drop behavior when no embedded match exists either), and
`bug040_generator_own_graph_order_still_wins_over_embedded_preset` (priority order preserved).
Still pure `Value → Value`; no consult of the live registry added; the baked table is
untouched.

### BUG-051 (trigger-clear-unwired) — `LiveTriggerState::clear()` never called; armed flags survive transport stop — FIXED 2026-07-07 @ 3089e0a3
**Status:** FIXED @ 3089e0a3

**Was:** `LiveTriggerState::clear()`'s own doc said "call on transport stop /
project reset so a stale 'fired, not yet re-armed' flag can't suppress the
first onset next time" (`live_trigger.rs:94-98`), but the engine's only use of
`live_trigger_state` was `.evaluate` in `tick_audio_triggers` — `clear()` had
zero call sites. Narrow in practice (flags re-arm on the next evaluate once
the impulse decays), but a real gap: transport stopping during an impulse
plateau and restarting while the same band was still hot would suppress the
first onset.

**Fix:** `PlaybackEngine::stop()` now calls `self.live_trigger_state.clear()`
directly, and the new `modulation::clear_all_trigger_edges(project)` (§8 P1,
`LIVE_AUDIO_TRIGGERS_DESIGN.md`) walks master effects, layer effects, and
generator instances clearing both the per-instance `audio_trigger.edge` (D2)
and every `is_trigger`-target `ParameterAudioMod.trigger_edge` (D5b) — the two
new §8 edge-state holders, folded into the same reset point per the fix shape
this entry originally specified. Regression proof:
`modulation::tests::clear_all_trigger_edges_rearms_generator_edge`.

### BUG-044 (mix-trigger-deafness) — Transient detection near-silent on dense full mixes — FIXED 2026-07-06 (novelty-vs-recent-max dual criterion; Sonnet agent build, orchestrator-verified)
**Status:** FIXED (2026-07-06)

**Was:** the adaptive threshold `median(ODF)×7+48` self-raised on dense productions
(continuous broadband change keeps the median elevated) — feel mix 1 Full fire in
11 s (drums stem: 32), apricots 2 (drums: 51), tears halved.

**Fix:** a genuine attack masked by a dense bed is admitted by a second, OR'd
criterion: `candidate > 2.0 × max(ODF over hops t−15..t−7) + 125`. A dense-but-
steady bed cannot inflate its recent MAX to kick size; every BUG-041 false-firer
(dive/riser/growl) spikes continuously so its recent max ≈ its peaks and novelty
never admits it (growl's ODF is a ~5-hop spike train to ~1259 — see observations).
Window excludes the candidate's own VQT-smeared rise (t−6..t) and the previous
16th-note hit (t−16). Median criterion untouched. Constants sit on a measured
plateau (factor ≥ 2.0, δ 48–300 all hold the zero-false-fire guards; sweep table
in the agent report, session 2026-07-06).

**Reproduction first:** new `densemix` scenario (three LFO'd supersaw clusters +
bright noise + 8 kicks; a static detuned bed contributes ~0 ODF — why busymix
never caught this — and the low cluster must sit inside the kick's sweep range).
Entry constants: 4 of 7 catchable kicks = gate FAIL; after: 7/7.

**Verified (orchestrator re-ran):** all selftest lines green (BUG-045's notes
87.6 unchanged, guards 0/0/0, kicks 8, busymix low 8, densemix low 7); feel mix
1→10, apricots 2→31, tears 35→60, inhale 45→58, bad_guy 61→82 Full fires;
on-grid ≥ 96%. Three brief retention caps (bad_guy mix ±30%, feel/apricots drums
±20%) exceeded by 2–3 fires: accepted — the caps were blunt proxies, the added
fires match real-hit magnitude (300–1600) and grid-align equal-or-better than
entry fires, and a six-family feasibility scan showed the caps jointly
unsatisfiable with tears ≥60 under ANY criterion shape. busymix Full went 0→7:
the P3 threshold had been over-suppressing genuine Full-band fires on sparse
mixes too.

**Follow-ups recorded, not done:** (a) consider a busymix/densemix FULL-band gate
once the right bound is understood (kicks full=9 vs low=8 needs explaining
first); (b) vocals stems got notably more sensitive (inhale vocals 29→49) —
plausibly real syllable onsets, no ground truth in the fixture set; check
against Peter's labeled clips when they arrive; (c) growl's spike-train ODF
means any future shortening of the median window below ~2 spike periods
resurrects BUG-041 — greppable warning lives here.

### BUG-042 (onset-settle-grab) — Tracker re-acquired garbage pitch during the post-attack settle window — FIXED 2026-07-06 (third design: position-anchored re-acquire window)
**Status:** FIXED (2026-07-06)

**Was:** D5's onset re-acquire teleported to `strongest_peak()` on the fire hop; the
VQT needs ~12 hops to settle post-attack, so the estimate was wrong ~70 ms on EVERY
note. Two prior fix shapes rejected with traces (instant teleport; zero-slack settle
window) — see the design doc P2c record.

**Fix (third design, honoring the measured 3-hop position / 12-hop strength split):**
an onset now OPENS a re-acquire window (CHALLENGE_HOPS long) instead of teleporting.
`pos` holds through the attack (correct for same-pitch re-attacks, the dominant real
case), continuation/takeover keep running (nothing freezes — rejected shape 2's flaw),
and the jump fires on position evidence: SETTLE_STREAK (3, plateau-swept 2/3/4 =
69.2/87.6/86.1) consecutive hops with the memoryless apex parked within MAX_SLEW of
the streak's ANCHOR (anchored, not hop-to-hop — the post-attack splash drifts 1–3
bins/hop and reads hop-to-hop-consistent), PLUS the apex must out-value the held
bin by CHALLENGE_RATIO — the window is an accelerated takeover clock (3 parked hops
instead of 12), never a lowered strength bar (without that clause a warm-up-artifact
fire teleported the dive 19 st onto a fade-in harmonic). Two sibling continuation
fixes shipped with it: super-slew+moving candidates are refused (hold, not
clamp-chase — kills the 7-st gap ring-down drag), and static peaks in the
MAX_SLEW..SLEW_RADIUS dead zone snap (tremolo-trough recovery; the hole the refusal
would otherwise open — wobble regressed 0.34→0.52 st before the snap, 0.39 after).
Also fixed: `gt_notes` claimed a phantom 19th note (synth writes 18) — 26
guaranteed-miss hops in the gate denominator.

**Verified:** notes accuracy 61.9→87.6 (gate 90 still red — the residual is a
DIFFERENT mechanism, filed as BUG-045), notes presence 43.6→100 PASS, octave-jump
gate PASS, all other selftest lines green. Real clips: tears bass (the oracle) 30→5
octave jumps; jumps drop across ~all 25 clips (bad_guy bass 26→13, vocals ~halved);
presence flat-to-up everywhere; apricots bass stays perfect (0 jumps, 0.83).

### BUG-043 (deep-bass-floor-anchor) — Tracker anchored at the spectrum bottom on deep sub-bass — FIXED 2026-07-06 (apex-masked salience comb)
**Status:** FIXED (2026-07-06)

**Was:** on real deep-sub stems (bad_guy, apricots bass) the Full/Low tracker sat at
10–18 Hz under the real ~40–80 Hz fundamental for whole clips; presence dark.

**Mechanism (pinned by the `sub` synthetic + column-level breakdown,
`sub_45hz_salience_argmax_on_fundamental_not_subharmonic_ghost`):** BOTH original
hypotheses, coupled. At the transform's bottom octaves the 4096-sample kernels are far
under-Q — a 45 Hz peak smears over ~40 bins at >50% magnitude — so a subharmonic
candidate's comb teeth (spaced only 8–14 bins) ALL land inside the one smeared mound:
h3 collects the true peak (ghost), h2/h4 collect its skirt (smear). Measured: S[15 Hz
ghost] 0.70 vs S[45 Hz true] 0.52. The memoryless salience argmax itself was wrong —
upstream of the tracker.

**Fix (at the mechanism):** the harmonic comb reads only spectral APEXES — `salience_into`
masks the column to local maxima ±`PEAK_MASK_RADIUS` (4 = half the minimum tooth
spacing) dilated ±1 bin, so a tooth landing on skirt collects 0. Restores the dominance
property that makes harmonic-sum salience correct: a sub-octave ghost collects each true
harmonic at strictly lower weight than the true fundamental does. Frequency-independent
(no fmin raise; a 22.5 Hz f0/2 ghost of a 45 Hz sub dies the same way).

**Follow-up the mask forced (riser presence-null regressed 100%→14.5%, fixed same
session):** sparse salience gave EVERYTHING neighbourhood contrast, so presence needed
two new multiplicative factors, both constant-free: **dominance** (`S[pos] / window max`
— presence requires being ON the window's dominant object; a tracker parked on residue
reads ~0) and **apex position-consistency** (window argmax within MAX_SLEW of last
hop's argmax — a real object's apex is self-consistent, band-noise's wanders; measured
10–20 bins/hop on the riser vs <0.3 on any real object, at every frequency). Dead ends
measured so they aren't retried: dominance² (pressure-tuned, still 88%), kernel-
normalized mound width (band-noise apex rides narrow chi-square structure — width does
NOT separate noise from tone). Riser's `distinct_full_acquisitions` gate became a
Schmitt counter (light display bar 0.25 / re-arm below 0.02) because presence now
legitimately hovers near the old 0.02 edge-count threshold on noise.

**Verified:** `sub` scenario gates 100%/100%; all selftest lines green except BUG-042's
known-failing notes-accuracy line; 25-clip scan — apricots bass median 66 Hz, 0 octave
jumps, presence 0.83 (was 3-bars-then-collapse); bad_guy/feel/tears/inhale bass at true
36–44 Hz fundamentals, presence 0.52–0.71; vocals/others unchanged. Side effect: notes
presence oracle (BUG-042's) went 43.6%→95.2% PASS; notes accuracy baseline moved
61.9%→56.4% (still the open BUG-042 target).

### BUG-041 (superflux-glide-fire) — Transients fire continuously through a pure pitch glide — FIXED 2026-07-06 (AUDIO_OBJECT_TRACKING P3)
**Status:** FIXED (2026-07-06)

**Symptom** (found 2026-07-06, mod_harness selftest) — the `dive` scenario (7-voice
supersaw gliding 1200→150 Hz, no attacks anywhere in the signal) lights the Transients
lane continuously in all bands: `docs/evidence/audio_modulation/selftest_dive.png`.
SuperFlux's frequency max-filter exists precisely to suppress pitch slides, and it
works for a single slide — the suspected mechanism is the supersaw's 7-voice detune
beating: per-harmonic amplitude modulation reads as genuine broadband dB flux that a
±1-bin max-filter (at bpo 24) cannot cover. Unconfirmed; needs the parameter sweep.

**Root cause:** unknown — suspects: `MAXFILTER_RADIUS` (1 bin) too narrow for detuned
stacks; `SUPERFLUX_DELTA`/threshold floor too low for dense sustained material
(`crates/manifold-audio/src/analysis.rs`, superflux consts ~line 540).

**Fix shape:** parameter sweep against the harness CSV gates (dive = 0 fires, kicks =
exactly 8, busymix ≥ 7 of 8) — owned by `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P3,
which carries the full brief. If no sweep point passes, that phase escalates with the
table rather than redesigning the detector.

**Blast radius grew 2026-07-06 (P2):** the false fires also break the D5 ridge
tracker — onset re-acquire (D5 step 4) teleports the tracked pitch on every false
fire, so P2's dive/wobble gates (max Δ 24 st, wobble stddev 7.25 st) are BLOCKED on
this bug. P3's exit gate now includes re-running the P2 gate lines to PASS.

**Fixed 2026-07-06** — root cause confirmed by the P3 parameter sweep (~150 configs):
the adaptive threshold was simply far too permissive for dense sustained material, not
the max-filter width (radius 1/2/3 indistinguishable). `SUPERFLUX_THRESH_FACTOR`
2.0→7.0, `SUPERFLUX_DELTA` 3.0→48.0 (mid-plateau: real kicks survive delta 30–300).
Result: dive/riser/growl 0 false fires, kicks exactly 8, busymix 8, and the P2
tracker gates all PASS (dive max Δ 0.38 st, wobble stddev 7.25→0.32 st) with NO D5
softening needed. ⚠ Sensitivity caveat: tuned on synthetics only — the raised
threshold makes the live Transients feature stricter everywhere; validate soft-onset
material (ghost notes, quiet hats) when Peter’s reference clips arrive.


All five entries below were fixed 2026-06-23, with a test per path:
- BUG-001–004 — commit `2e3dc4f3` (`PresetInstance::duplicated()`, both paste paths, `Clip::clone_with_new_id`, `Layer::clone_with_new_ids`).
- BUG-005 — commit `9f43f183` (macros address effects by `EffectId`; versioned load migration).

The fresh-copy carry-rule (id always fresh; drop Ableton/MIDI + audio mods; drop cross-chain group; keep drivers/envelopes) is settled and lives in `PresetInstance::duplicated()`.

### BUG-001 — Pasting an effect shares the source's `EffectId` — HIGH — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Copy/paste of an effect card clones the `PresetInstance` verbatim and keeps the original's
`EffectId`. Nothing mints a fresh id. The two cards then share one identity, and the whole
system addresses effects by id with **first-match-wins** resolution, so they collide.

**Root cause**
- Clipboard clones verbatim: [clipboard.rs:32-34](../crates/manifold-editing/src/clipboard.rs#L32-L34) (`get_paste_clones` is a bare `.clone()`; `.clone()` copies the `id` field).
- Paste path 1: [input_host.rs:263-273](../crates/manifold-app/src/input_host.rs#L263-L273) (`handle_effect_paste`) — feeds the clone to `AddEffectCommand`, no `regenerate_id()`.
- Paste path 2: [app_render.rs:1907-1918](../crates/manifold-app/src/app_render.rs#L1907-L1918) (PanelAction paste) — same omission.

**Symptom (user-visible)**
- Move a slider on one card → the other card's value moves too.
- Undo/redo of an edit to one card hits the other (or the wrong one).
- The two cards share GPU/visual state (feedback trails, sim buffers) — see blast radius below.

**Why each symptom happens**
- Edits resolve via `Project::find_effect_by_id_mut` ([project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947)) and `set_base_param_by_id` — first match by id wins, so card B's edit lands on card A.
- Undo/redo commands store an `EffectId` and re-resolve the same way.
- The renderer's per-frame chain rebuild `harvest_state_from` ([preset_runtime.rs:1667-1743](../crates/manifold-renderer/src/preset_runtime.rs#L1667-L1743)) matches cards by first-match `EffectId` (lines 1684, 1697-1701). Two same-id slots in one chain both match the *same* prior slot → GPU node impls + `StateStore` buckets migrate to the wrong/shared card.

**Correct pattern to mirror**
`Layer::clone_with_new_ids` already does this right — it calls `effect.regenerate_id()` on
every cloned effect ([layer.rs:886-900](../crates/manifold-core/src/layer.rs#L886-L900)).
`PresetInstance::regenerate_id` is at [effects.rs:1768](../crates/manifold-core/src/effects.rs#L1768).

**Fix shape**
Call `fx.regenerate_id()` before building the `AddEffectCommand` in both paste paths. Decide
the `group_id` question (see BUG-003) and the carried-binding question (see BUG-004) in the
same pass. Add a paste test mirroring the graph-node one.

**Test:** none yet. Add `effect_paste_assigns_fresh_id` to `manifold-editing`.

---

### BUG-002 — `Clip::clone_with_new_id` doesn't regenerate nested effect ids — MED — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Same class as BUG-001, one layer down. `Clip::clone_with_new_id` mints a fresh `ClipId` but
bare-`.clone()`s everything else, including `effects: Vec<PresetInstance>`
([clip.rs:105](../crates/manifold-core/src/clip.rs#L105)). So a duplicated clip's effects keep
the **source clip's** `EffectId`s. Clip effects share the same first-match namespace
([project.rs:938-944](../crates/manifold-core/src/project.rs#L938-L944)).

**Root cause**
[clip.rs:168-172](../crates/manifold-core/src/clip.rs#L168-L172) — shallow clone of nested effects.

**Every clip-duplication path inherits it** (all funnel through that one function):
- Paste clip — [service.rs:452](../crates/manifold-editing/src/service.rs#L452)
- Duplicate clip — [service.rs:740](../crates/manifold-editing/src/service.rs#L740)
- Split clip (overlap-driven + explicit) — [layer.rs:616](../crates/manifold-core/src/layer.rs#L616), [SplitClipCommand](../crates/manifold-editing/src/commands/clip.rs#L599)
- Trim / copy-in-region — [service.rs:628](../crates/manifold-editing/src/service.rs#L628)
- Duplicate layer — [layer.rs:871](../crates/manifold-core/src/layer.rs#L871) (clones clips, never touches their effect ids)

**Symptom**
Editing an effect on a duplicated/split clip crosstalks with the source clip's effect.
**Split is the surprising trigger** — a user doesn't think of splitting a clip as
"duplicating," but it produces two clips silently sharing effect ids.

**Scope note:** only bites clips that carry effects (effects usually sit on layers, so this is
the less-traveled path — hence MED, not HIGH). Renderer state does **not** collide across
clips: clip chains have distinct `OwnerKey` per clip ([state_store.rs:30-34](../crates/manifold-renderer/src/node_graph/state_store.rs#L30-L34)), so the model-layer collision is the whole bug here.

**Fix shape**
Make `Clip::clone_with_new_id` deep-regenerate `cloned.effects[*].id` (and clip-effect
`group_id` if any). One function fixes all six entry points, including the layer-dup gap.

**Test:** none yet. Add `clip_clone_assigns_fresh_effect_ids` to `manifold-core`.

---

### BUG-003 — Duplicating a grouped effect leaves `group_id` pointing at the source's group — LOW — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

A pasted/duplicated effect keeps its `group_id`, which still references a group on the
**source's** chain. `Layer::clone_with_new_ids` remaps this for layer effects
([layer.rs:889-893](../crates/manifold-core/src/layer.rs#L889-L893)), but the effect-paste
path (BUG-001) and the clip-effect path (BUG-002) don't. Fixing BUG-001/002 by regenerating
ids must also decide the `group_id` remap, or you trade an id collision for a dangling group
ref.

**Status:** rolled into the BUG-001/BUG-002 fix; tracked separately so it isn't forgotten.

---

### BUG-004 — Effect paste carries Ableton/automation bindings; generator paste drops them — LOW — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Effect paste clones the whole `PresetInstance`, so `ableton_mappings`, `drivers`, `envelopes`,
and `audio_mods` all ride along — a pasted effect ends up mapped to the **same Ableton
control** as the source, and one knob drives both. Generator paste does the opposite: its
`GeneratorSnapshot` carries `drivers` + `envelopes` but **not** `ableton_mappings` or
`audio_mods` ([clipboard.rs:54-95](../crates/manifold-editing/src/clipboard.rs#L54-L95)).

This is an inconsistency, not strictly a crash. Per the effect/generator binding-parity
principle the two paste paths should agree. Decide the intended behavior (most DAWs do **not**
carry hardware/MIDI mappings onto a paste) and make both paths match.

**Status:** design decision to settle alongside BUG-001.

---

### BUG-005 — Macro targets can't disambiguate two same-type effects on one layer — LOW — ✅ FIXED (`9f43f183`)
**Status:** FIXED

`MacroMappingTarget` addresses an effect param by `(layer_id | master, effect_type, param_id)`
([macro_bank.rs:64-82](../crates/manifold-core/src/macro_bank.rs#L64-L82)) — **not** by
`EffectId`. So duplicating an effect (trivially producing two `Blur`s on one layer) makes any
macro mapping to that `(layer, Blur, param)` ambiguous; resolution can't tell the copies
apart. Distinct from the id-collision class (macros are immune to that because they don't key
on `EffectId`), but the same root trigger — duplication — exposes it.

**Fix shape:** address macro targets by stable `EffectId` like single-card edits already do
(`docs/CARD_TARGET_UNIFICATION.md`). Larger than a one-liner; parked here so it's recorded.

---

## Checked and safe (coverage proof)

Audited during the 2026-06-23 duplication sweep; these duplicate correctly. Recorded so the
audit boundary is auditable.

- **Graph-node copy/paste** — `PasteNodesCommand` ([graph.rs:1985-2110](../crates/manifold-editing/src/commands/graph.rs#L1985-L2110)) mints fresh runtime ids + fresh `NodeId`s, remaps internal wires, starts pasted nodes un-exposed. Has regression tests (`paste_node_clones_with_fresh_identity_and_undo_removes`, `paste_remaps_internal_wires_to_the_new_node_ids`). **This is the reference implementation** for the BUG-001/002 fixes.
- **Generator paste** — `PasteGeneratorCommand` overwrites the target layer's single generator in place, addressed by `LayerId`. No id minted, no collision.
- **Markers** — created fresh via `TimelineMarker::new` (fresh `MarkerId`, [marker.rs:20-27](../crates/manifold-core/src/marker.rs#L20-L27)); no copy/paste/duplicate-marker path exists (markers are timeline-level, untouched by layer/clip dup).
- **New-clip-from-scratch paths** (MIDI/percussion/live-trigger/browser-drop) — construct fresh clips, not duplicates of existing ones.

## Blast radius — id-keyed resolvers that a duplicate `EffectId` breaks

All first-match-wins; all used by both editing and undo/redo:
- `Project::find_effect_by_id_mut` — [project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947) (master + layer + clip effects)
- `Project::find_effect_by_id` — [project.rs:711](../crates/manifold-core/src/project.rs#L711)
- `GraphTarget::Effect` / `set_base_param_by_id` paths that wrap them
- Renderer chain rebuild `harvest_state_from` — [preset_runtime.rs:1667](../crates/manifold-renderer/src/preset_runtime.rs#L1667) (per-card GPU state migration)

**Not** in the blast radius: macros (`(layer, type, param)`-addressed — see BUG-005),
markers, generators (`LayerId`-addressed).

## The pattern behind all of this

Duplicating an id-bearing entity must mint a fresh identity for itself **and** every nested
id-bearing child, or id-keyed first-match resolution collides. The graph-node path enforces
this with a test and never regressed; the paths without a test (effect paste, clip clone)
did. The durable fix for the class is a test per duplication path, not a doc note.

Related agent-memory notes: `feedback_hidden_field_dependencies` (the mirror — removing a
field silently breaks identity), and `project_invariant_audit` (its "Positional identity"
category is marked *already fixed*; BUG-001/002 are live counterexamples — correct that claim
when one is fixed).

