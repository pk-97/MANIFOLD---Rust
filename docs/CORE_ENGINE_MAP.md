# Core Engine — Current-State Map (Playback · Control · Timeline · MIDI · OSC · Timecode)

<!-- index: AUTHORITATIVE current-state map of the core playback engine and everything that drives it (2026-07-03, from a full code read): the content-thread frame, the time model, clock authority + SyncArbiter, sync_clips_to_time and its three ref sources (timeline / live slots / session), the MIDI + OSC + Link + Ableton sync stacks, modulation, tempo recording, the full threshold table, test surface, and the honest open edges. The companion of FREEZE_COMPILER_MAP.md, for the other half of the machine. -->

**Status: AUTHORITATIVE map of what is actually in the code, written 2026-07-03
from a full read of `manifold-playback`, the timeline/tempo model in
`manifold-core`, and the content-thread plumbing in `manifold-app`.** This is
the doc to read before touching transport, scheduling, sync, MIDI, OSC, or
timecode — and the doc a bug hunt attacks (§13). It is the sibling of
`FREEZE_COMPILER_MAP.md`: that doc maps what turns graphs into pixels; this one
maps what decides *which clips are alive and what time it is*.

Written at `feat/timeline-ui-redesign` immediately after SESSION_MODE P2 merged
(`f852d2bc` — `SessionRuntime` as the third ref source). Sections marked (P2)
are that fresh.

---

## 1. What it is, in one paragraph

The content thread runs a fixed-rate loop (project FPS, default 60). Each tick
it drains UI commands, polls every external sync source (MIDI Clock, Link, OSC,
Ableton), derives the current beat, ticks the `PlaybackEngine` — which diffs
"what should be playing" against "what is playing" via `sync_clips_to_time`,
starts/stops clip renderers, runs the modulation pipeline — then renders and
pushes a `ContentState` snapshot to the UI. Time is **beats first**: the engine
integrates seconds (f64) and converts to beats through the project's
`TempoMap`, except when an external clock (MIDI Clock, Link) *is* the beat, in
which case the beat is set directly and seconds are derived from it. One
structural gatekeeper (`SyncArbiter`) decides who may touch the transport.

**For the instrument:** this is the machinery that makes a MIDI pad hit land on
the beat, keeps video locked to Ableton for a whole set, and decides what is on
screen at every moment of a show. Its failure modes are timing failures — a
launch that lands off-grid, a playhead that drags backward after a scrub, a
clip that black-frames on its first hit. Every threshold in §11 exists because
one of those happened.

## 2. File map

**`manifold-playback` (the engine crate):**

| File | Role | Size |
|---|---|---|
| `engine.rs` | `PlaybackEngine`: transport state, the tick, `sync_clips_to_time` (sole authority), clip lifecycle, drift correction, prewarm, rate/loop math, session commands (P2), `LiveClipHost` + `SyncTarget`/`SyncArbiterTarget` impls. | 2547 |
| `scheduler.rs` | `ClipScheduler::compute_sync` — the pure diff: (timeline refs + live refs [+ session refs merged by caller]) vs active ids → `to_start`/`to_stop`. `ActiveClipRef` (lightweight, Arc-str id). Zero-alloc reclaim cycle. | 466 |
| `sync.rs` | `SyncTarget` (read) / `SyncArbiterTarget` (write) split + `SyncArbiter`: authority gate, `suppress_next_transport`, `manifold_owns_playback` + 0.5s grace, 0.3s user-seek cooldown. | 235 |
| `sync_source.rs` | `SyncSource` trait (enable/disable/toggle). | 16 |
| `session_state.rs` | (P2) `SessionRuntime`: playing/pending/`session_override` per layer, quantize boundary math, `resolve_refs` (third ref source, stateless from `current_beat`), wrap-restart detection, global-beat rebasing. | 772 |
| `live_clip_manager.rs` | Phantom clips: NoteOn creates a live slot, NoteOff commits. Quantize math (24 ticks/beat), 5ms NoteOff guard, pending-launch queue (tick-keyed), audio one-shot expiry, recording provenance capture. | 1169 |
| `clip_launcher.rs` | MIDI note → layer resolution (SingleNote/AllNotes, channel/device filters), random clip pick (deterministic when seeded by event sequence), in-point randomize, NoteOff tracking keyed `(note, device_id)`. | 679 |
| `live_trigger.rs` | Audio transient → one-shot fires. Pure edge detection with 0.6 re-arm hysteresis (upstream onset detector owns the refractory). | 190 |
| `midi_input.rs` | midir note input (replaces Unity Minis + native plugin). Per-port callback → mpsc → per-tick drain (cap 512), deterministic sort (tick, NoteOff-before-NoteOn, sequence). `absolute_tick` is **always −1** on this path. | 845 |
| `midi_clock_sync.rs` | MIDI Clock/SPP receiver (lock-free `AtomicU64` CAS state in the midir callback) + controller: 0.5s activity timeout, BPM EMA from tick rate, nudge-vs-hard-seek position sync, seek-cooldown suppression. | 833 |
| `midi_parser.rs` | Minimal SMF parser (format 0/1, PPQ only) → beat-domain notes. | 330 |
| `midi_import.rs` | MIDI-file → timeline clips. | 193 |
| `link_sync.rs` | Ableton Link via rusty_link: beat/phase/tempo poll, start-stop sync → arbiter play/pause. | 179 |
| `osc_receiver.rs` | UDP OSC listener: background thread → latest-message-per-address queue → main-thread dispatch to subscribers. | 358 |
| `osc_sync.rs` | OSC/SMPTE timecode controller (LiveMTC): drop-frame conversion, timecode-activity transport follow, nudge/seek. **Receive path currently unwired — see §13.1.** | 428 |
| `osc_sender.rs` | `/manifold/play|transport|position` to M4L: 3× redundant sends, 0.3s re-confirm window, echo suppression via arbiter flag. | 311 |
| `osc_param_router.rs` | Data-driven OSC float → param writes (macros, master/layer effects, generator params, opacity). Rebuilt on structural change from command handlers. | 336 |
| `ableton_bridge.rs` | AbletonOSC (11000/11001): session discovery, macro listeners → param writes, cue points + PLAY-group arrangement for the perform HUD, outbound transport (mirror of osc_sender). Inbound transport relay **disabled** (echo loops). | 3204 |
| `transport_controller.rs` | Authority cycling + Link/CLK/OSC toggles, BPM edit commands (via EditingService — the only tempo write that is undoable). | 359 |
| `tempo_recorder.rs` | External-tempo recording: session lifecycle, point spacing (≥0.125 beats, ≥0.05 BPM delta), tempo-lane snapshot into provenance. | 339 |
| `modulation.rs` | Per-frame pipeline: reset effectives → LFO drivers → audio mods → clip-triggered decay envelopes. Per-instance envelopes (post-unification). | 939 |
| `video_time.rs` | Pure video-time math (in-point, looping, rate) — Unity-exact. | 116 |
| `active_window.rs` | Incremental active-clip window (boundary cursors). **Dead code — never instantiated; see §13.8.** | 680 |
| `renderer.rs` | `ClipRenderer` trait (video pool / generator / image implement it) + `StubRenderer` for tests. | 202 |
| `audio_sync.rs` / `audio_warp.rs` / `audio_layer_playback.rs` / `audio_mixdown.rs` | Per-clip audio decode (kira) + encoder-delay probe, warp, transport-following playback, offline mixdown. | ~1.1k |
| `percussion_*.rs`, `process_runner.rs` | Percussion import pipeline (external process). Not core transport; not mapped further here. | ~4.4k |

**`manifold-core` (the model the engine reads):**

| File | Role |
|---|---|
| `units.rs` | `Beats`(f64) / `Seconds`(f64) / `Bpm`(f32) newtypes. The engine's beat accumulator is f64; several boundaries still round-trip f32 (§13.6). |
| `tempo.rs` | `TempoMap` (sorted points, step-change BPM) + `TempoMapConverter` (piecewise beat↔seconds integration, f64 variant for the per-frame path, BPM clamped 20–300). Linear walks (§13.9). |
| `timeline.rs` | `Timeline`: layers, self-healing clip-id lookup cache, `enforce_tree_order` (pre-order DFS group invariant), mute/solo-aware `get_active_clips_at_beat_ref`, markers. |
| `layer.rs` | `Layer`: sorted clip caches, **`enforce_non_overlap_for`** — DaVinci-style write-time overlap resolution (delete / trim-start with in-point advance / trim-end / split), returning `OverlapAction`s for undo. |
| `clip.rs` | `TimelineClip`: beat-domain position/duration, `in_point` (Seconds), loop fields, `recorded_bpm`, absolute-tick provenance. |
| `math.rs` | `BeatQuantizer`: BPM step 0.01, beat step 0.0001, time step 0.0001 — save-file jitter suppression. |
| `session.rs` | (P2) `SessionGrid` / `Scene` / `SessionSlot` / `ClipSequence` — the serialized grid the runtime resolves against. |

**`manifold-app` (the orchestration):**

| File | Role |
|---|---|
| `content_thread.rs` | The loop: real-time thread policy, command drain with seek coalescing, surface wait, `tick_frame` (order in §3), `tick_sync_controllers`, `derive_external_beat`, `apply_resolved_tempo`, state push. |
| `content_commands.rs` | `handle_command`: transport (Play aligns to CLK beat, claims ownership, sets seek cooldown), editing, session gestures, project lifecycle, OSC router rebuilds. |
| `content_command.rs` | The `ContentCommand` enum — the complete UI→content surface. |
| `content_state.rs` | `ContentState` snapshot — everything the UI knows per frame. |
| `content_pipeline.rs` | GPU side (compositor/chains) — mapped in FREEZE_COMPILER_MAP.md, not here. |

## 3. The frame, end to end

Order is load-bearing; it ports Unity's execution order and the comments pin it.

```
timer.wait_for_deadline()                 (mach_wait_until + 2ms spin)
1. drain ContentCommands                  (seeks coalesced to the last one)
2. wait for GPU surface                   (draining commands while blocked)
3. tick_midi_input                        (drain midir notes → ClipLauncher → LiveClipManager)
3b. tick_sync_controllers
      auto-detect ClockAuthority          (CLK-receiving > OSC-timecode > Link-with-peers > Internal;
                                           written into project.settings each frame — §13.4)
      Link.update    → arbiter play/pause, live tempo (priority 1)
      MidiClock.update → external_time_sync gate, transport, nudge/seek position, live tempo (priority 2)
      OscReceiver.update → dispatch subscribers → OscParamRouter.apply
      AbletonBridge.update/apply          (macro writes; sets ableton_active flag)
      OscSync.update (M4L mode)           (timecode transport + position — currently starved, §13.1)
3c. derive_external_beat                  (CLK: beat := clock beat unless seek-cooldown;
                                           Link: beat := link beat − cached offset unless manifold owns)
    update_recording_session_state / apply_resolved_tempo
                                          (recording: append tempo points; else beat-0 point when Internal)
3d. audio_mod_runtime.update              (fill engine.audio_snapshot)
4. engine.tick(ctx)
     playing branch:
       consume sync-dirty → advance_time (unless external_time_sync) → sync_project_bpm
       → activate due pending live launches → audio triggers (fire/expire one-shots)
       → sync_clips_to_time               (THE authority — see §5)
       → update_active_clip_playback_rates → check_custom_loop_boundaries
       → evaluate_modulation → correct_video_drift (every 2s, not in export)
       → filter_ready_clips → compute_prewarm_candidates
     non-playing branch: flush sync-dirty (sync + seek_active_clips), rates, modulation, filter
4b. transport out (AbletonBridge or OscPositionSender late_update — echo-suppressed)
5. audio_layer_playback.update            (kira voices follow transport)
6. percussion tick · video prewarm handoff
7. render_content → cleanup stopped clips' GPU state → LED output
8. push ContentState                      (project snapshot only on data_version change;
                                           modulation frames send a flat ModulationSnapshot)
```

## 4. Time model & tempo

- `current_time_double: f64` is the integrator; `current_beat: f64` is derived
  through `TempoMapConverter::seconds_to_beat_f64` on every advance/seek —
  **except** under an external beat authority, where `set_beat` +
  `sync_time_from_beat` invert the direction. `external_time_sync = true`
  suppresses local `advance_time` entirely; the playhead then moves only when
  the external source says so.
- `TempoMap` is a sorted list of step-change points; conversion is piecewise
  linear integration. All BPM clamps to 20–300 everywhere (converter, engine,
  recorder). `BeatQuantizer` quantizes any BPM/beat/time that lands in the
  save file, so float jitter can't dirty it.
- Live external tempo (Link first, then MIDI Clock) is held in
  `engine.live_external_tempo` and consulted *before* the tempo map by
  `get_bpm_at_beat` / `get_seconds_per_beat_at_beat` — this drives video
  playback *rates* and BPM display regardless of authority (§13.5).
- `sync_project_bpm_from_current_beat` runs every tick and writes
  `project.settings.bpm` (quantized, 0.005-thresholded) from live tempo or the
  map. It is a display/rate convenience, not an undoable edit.
- Recording: `TempoRecorder` appends map points while armed (recording +
  playing + authority ≠ OSC) with ≥0.125-beat spacing and ≥0.05 BPM delta; on
  disarm it snapshots the whole lane into `RecordingProvenance`. Non-recording
  + Internal authority + trivial map: external tempo writes the beat-0 point
  as a "global master tempo". After any map change the beat is re-derived from
  time.

**For the instrument:** beats are the contract. A clip drawn at bar 33 plays at
bar 33 under any tempo automation; a BPM change rescales the timeline through
an undoable command (`RescaleBeatsForBpmChangeCommand`), never silently.

## 5. `sync_clips_to_time` — the sole authority

Everything that starts or stops a clip goes through this one idempotent
function (invariant since the Unity port; P2 kept it — `SessionRuntime` feeds
it rather than bypassing it):

1. `query_active_timeline_clips` — mute/solo/group-aware timeline query at
   `current_beat` into scratch (skipped for session-overridden layers, P2).
2. `resolve_session_refs` (P2) — promote due pending launches, resolve playing
   slots to global-beat `ActiveClipRef`s, stop wrap-restart evictions.
3. `fill_live_slot_refs` — phantom clips (NoteOff-lifetime: merged when
   `current_beat ≥ start_beat`, **never expired by end_beat** — the video may
   freeze on last frame but the slot lives until NoteOff commits the true held
   duration).
4. `ClipScheduler::compute_sync` — pure diff against `active_clip_ids`;
   micro-clip guard: don't start non-looping clips with < 0.02s-worth of beats
   remaining.
5. Stops via `stop_clip` (renderer stop + tracking cleanup + stopped-clips list
   → GPU per-owner state release), starts via `start_clip` (renderer dispatch
   by `can_handle`, group layers refused, pending-pause for video, recently-
   started gate entry).

Deferral: MIDI/live events set `sync_clips_dirty` instead of syncing inline;
the tick consumes it (playing: implicit — sync runs anyway; stopped/paused:
sync + `seek_active_clips`, which is what makes scrub-while-stopped render).

Clip start → ready is a state machine for video: `start_clip` → (prepare
phase) `preparing_clips` → `check_preparing_clips` polls readiness → seek to
computed video time, apply BPM-stretch rate, resume, enter
`recently_started_times` (compositor exclusion 0.1s / 0.02s live, capped at
40% of remaining clip time). Drift correction every 2s re-seeks players > 0.1s
off expected source time, enforces out-points, restarts stalled players; live
slots are exempt from seek-correction (they loop natively).

## 6. Live layer — MIDI notes, audio triggers, session grid

**MIDI note path:** midir callback (any thread) → mpsc → per-tick drain (≤512)
→ deterministic sort → `ClipLauncher`. Layer-based routing first
(channel/device filters, SingleNote match or AllNotes drum-pad mode), then
`MidiMappingConfig` fallback. NoteOn: resolve layer content (generator ↔ video
folder, one rule in `resolve_layer_live_content`), pick a clip (seeded-random,
no immediate repeat), create a phantom clip on the live slot
(`recorded_bpm` stamped for rate math), track for NoteOff under
`(note, device_id)`. NoteOff: 5ms/sequence stale guards, commit — truncate to
held duration (quantized), auto-loop if held past the source length, and if
recording, `AddClipCommand` into the timeline (undoable, overlap-enforced) +
provenance finalization.

**Audio triggers:** engine tick evaluates `LiveTriggerState` against the
audio-feature snapshot; fires beat-domain one-shots through the *same*
`fire_layer_oneshot` → `trigger_live_clip` primitives (beat-stamped, tick −1 —
real-time snap at the playhead), expiry by `end_beat` since transients have no
NoteOff.

**Session grid (P2):** `SessionRuntime` is runtime-only state (never
serialized, never undo-wrapped). Launch/stop are *pending* entries targeting
the next quantize boundary (default 4 beats; 0 = now; launching from stopped
transport = immediate). Activation flips `session_override` on the layer —
the layer detaches from the arrangement and stays detached (through stop-slot,
stop-all, and transport stop) until an explicit Back to Arrangement. Slot
resolution is stateless from `current_beat` (seek-proof); the one wrinkle —
a loop wrap where the same inner clip spans the boundary — is detected via
`last_iteration` and force-restarted through the engine's own `stop_clip` so
`compute_sync` restarts it from `in_point`. Inner clip `start_beat`s are
sequence-relative and are **rebased to global beats** before they touch any
video-time math.

## 7. External sync — who is allowed to move the playhead

`SyncArbiter` is the structural gate: every sync controller passes
`(source, authority)` and the call is dropped unless they match. Authority is
**auto-detected each frame** (§3) — the CLK/Link/OSC toggles control *sources*,
not authority.

- **MIDI Clock** (one-way, DAW → MANIFOLD): 0xF8 ticks accumulate
  sixteenths+tick in a lock-free packed `AtomicU64` (the OS MIDI thread never
  blocks); SPP sets position; Start/Continue/Stop drive transport. Position
  sync: nudge when |Δ| < 2.0s and tick-delta sane (0..=384), hard seek
  otherwise; while stopped, seek on sixteenth change. BPM estimated over ≥96
  ticks, EMA α=0.30. Two round-trip protections: `manifold_owns_playback`
  (MANIFOLD initiated play via OSC; CLK may not clear it for 0.5s) and the
  0.3s user-seek cooldown (CLK position sync suppressed while Ableton catches
  up to a scrub). Beat is derived directly (`derive_external_beat`).
- **Link**: tempo always (priority over CLK); position only when MANIFOLD does
  *not* own playback, via `link_beat_offset` cached at Play/Seek (Stop poisons
  it with NaN). Start-stop sync maps to arbiter play/pause.
- **OSC timecode (SMPTE via LiveMTC, M4L mode)**: 4-float H:M:S:F (or 1-float
  seconds) → drop-frame-correct seconds; timecode arriving = play, 0.5s
  silence = pause; playing: nudge every message (<0.5s) else seek; stopped:
  seek beyond 0.05s. **Currently starved — no subscription wires the receiver
  to `on_timecode_received` (§13.1).**
- **Outbound** (MANIFOLD → Ableton): `OscPositionSender` (M4L) or
  `AbletonBridge` transport (AbletonOSC mode) — play carries the beat, stop is
  `/transport 0`, seeks fire on >0.5-beat divergence from dead-reckoned
  position; 3× redundant sends + 0.3s confirm window; echo suppressed via
  `suppress_next_transport`. AbletonOSC *inbound* transport is disabled
  pending echo-loop investigation (code TODO).
- **Ableton bridge** additionally: session discovery (tracks/racks/macros),
  macro listeners → replace-mode param writes each frame (flagged so the UI
  snapshot follows), cue points + PLAY-group arrangement for the perform HUD.

## 8. Modulation pipeline (per tick, all transport states)

`reset_all_effectives` (base → value) → LFO drivers (set) → audio mods (set,
follower-shaped) → decay envelopes (additive, level = pure function of
beats-into-active-clip, per-instance since envelope-home unification — two
same-type effects no longer collide). Muted layers still modulate (inspector
liveness); disabled effects don't. Orphaned references (deleted send, missing
param) leave the base value — no fallback writes. Any write marks the
compositor dirty and triggers a `ModulationSnapshot` push (flat buffer, no
project clone).

## 9. Threads & channels

- Content thread: `THREAD_TIME_CONSTRAINT_POLICY` (real-time, 75% computation
  budget), QoS fallback. Owns engine + project + all controllers.
- UI → content: bounded(64) `ContentCommand`; content → UI: `ContentState`
  (unbounded, never drops). Seeks coalesce in the drain loop.
- midir note callback → mpsc → drained in-tick. MIDI Clock state → atomic CAS,
  no queue. OSC UDP → background thread → mutex'd latest-per-address map →
  main-thread dispatch (intermediate values per address are dropped by
  design). Ableton bridge has its own recv thread + pending-write mutex.
- Zero-alloc discipline on the tick: scheduler buffer reclaim,
  `TickResult` reclaim, scratch vectors on the engine, `Arc<str>` state
  strings. (Where it's violated, see §13.7.)

## 10. Write-time invariants (the model's contracts)

- **Non-overlap** is enforced when clips are placed/moved
  (`enforce_non_overlap_for`): full cover → delete, partial → trim (video
  in-point advanced by the trimmed seconds), containment → split; every action
  returned for undo. `restore_clip` is the only raw-insert door (undo paths).
- **Tree order**: a group's children are contiguous after it
  (`enforce_tree_order`, orphans promoted, debug-asserted).
- **Lookup caches** (clip-id map, layer-id map, per-layer sorted orders) are
  runtime-only, dirty-flagged, self-healing on miss.
- All persistent mutation flows through `EditingService` commands — the
  exceptions on the engine tick (settings.bpm, clock_authority, OSC/Ableton
  param writes, modulation effectives) are deliberately *live* state, but two
  of them land in serialized fields (§13.4).

## 11. The threshold table

Every magic number on the timing paths, in one place:

| Constant | Value | Where / why |
|---|---|---|
| `PENDING_PAUSE_DELAY` | 0.1s | video pause-after-prepare settle |
| `RECENTLY_STARTED_TIME` / live | 0.1s / 0.02s | compositor exclusion until first decode (≤40% of clip remainder) |
| `COMPOSITOR_DIRTY_TIME` | 0.05s | dirty-deadline window |
| `MIN_START_REMAINING_TIME` | 0.02s | micro-clip start guard (beat-domain) |
| clip rate clamp | 0.05–8.0 | BPM time-stretch bounds |
| BPM clamp | 20–300 | everywhere |
| `video_sync_interval` | 2.0s | drift-correction cadence (off in export) |
| drift re-seek threshold | 0.1s | player vs expected source time |
| `NOTE_OFF_TIMING_GUARD` | 5ms | Minis-style instant NoteOff rejection |
| live-slot clear on seek | >1.0 beat | big jump clears phantom clips + pending |
| `SEEK_COOLDOWN` | 0.3s | CLK position sync suppressed post-scrub |
| `OWNERSHIP_GRACE_PERIOD` | 0.5s | manifold-owns can't be cleared early |
| CLK `clock_signal_timeout` | 0.5s | receiving → not-receiving |
| CLK nudge/seek split | 2.0s | absorb tempo ramps, catch real jumps |
| CLK tick-delta sanity | 0..=384 | SPP jump detection |
| CLK BPM estimate | ≥96 ticks, EMA 0.30 | tempo readout smoothing |
| OSC `transport_timeout` | 0.5s | timecode silence → pause |
| OSC nudge/seek split | 0.5s | playing-state position handling |
| OSC stopped `seek_threshold` | 0.05s | paused churn guard |
| OSC/Ableton transport sends | 3× + 0.3s confirm | UDP loss on localhost under load |
| Ableton echo window | 0.5s | inbound is_playing ignored post-send |
| `SEEK_THRESHOLD_BEATS` (out) | 0.5 beats | dead-reckoned seek detection |
| trigger `REARM_RATIO` | 0.6 | transient hysteresis |
| session quantize default (P2) | 4.0 beats | launch boundary |
| `MIDI_CLOCK_TICKS_PER_BEAT` | 24 | quantize/provenance tick domain |
| tempo point spacing | 0.125 beats / 0.05 BPM | recorder density |

## 12. Test surface & how to debug

- Integration (`crates/manifold-playback/tests/`): `engine_tick.rs` (8 — init,
  stopped/playing ticks, scheduling at beats, 1000-frame soak, seek, beat↔time
  round-trip, waypoint stress; drives the real engine with `StubRenderer`s),
  `live_clip.rs` (19 — NoteOn/NoteOff lifecycle), `session_mode.rs` (5, P2).
- Inline units: scheduler (13), session_state (20), modulation (15 —
  characterization + audio mods + range overrides), live_trigger (5),
  video_time (6), osc_sender encoding (2), active_window (7 — **tests of dead
  code**), core: clip (21), layer (9 — overlap), timeline (7 — tree order),
  tempo (2).
- **Zero tests**: `SyncArbiter` gating matrix, `midi_clock_sync` (the packed
  atomic, BPM estimator, nudge/seek logic), `link_sync`, `osc_sync` (timecode
  math incl. drop-frame), `osc_receiver`, `osc_param_router`,
  `transport_controller`, `tempo_recorder`, engine drift correction and
  custom-loop boundaries. The entire external-sync stack is verified only on
  stage (§13.2).
- Scope: `cargo test -p manifold-playback` is seconds. The canonical
  load-bearing fixture is `Liveschool Live Show V6 LEDS.manifold`.
- Debugging timing bugs: this is callback/event-ordering territory — add
  `println!`/`log`, reproduce, read. `show_debug_logs` flags exist on
  MidiInput/ClipLauncher/OscSync; CLK/Link/OSC log transport transitions and
  sync established/lost by default. Profiling feature records per-phase
  content timings.

## 13. Honest edges (the bug hunt starts here)

1. **The OSC/SMPTE timecode receive path is dead.** Nothing ever calls
   `OscSyncController::on_timecode_received` and `enable_osc` never registers
   a subscription (the code's own TODO says "when native OSC is live,
   subscribe via subscribe_keyed"). Consequences: `is_receiving_timecode`
   can never become true, so timecode transport-follow, position sync, *and*
   the OSC arm of authority auto-detection are all inert. M4L LiveMTC sync —
   a first-class feature of the Unity rig — silently does nothing in the Rust
   port. (`timecode-locks-score-not-render` work must land on top of a wired
   path first.)
2. **MIDI launch quantization is inert on the midir path.** midir events
   always carry `absolute_tick = −1` → `beat_stamp = None`, and the engine's
   `beat_snapped_beat_resolver` / `absolute_tick_resolver` delegates are never
   installed — so `compute_trigger_snap_beat` falls through to
   `get_beat_snapped_beat()` = **raw current beat, unsnapped**. QuantizeMode
   still shapes *durations* (`compute_duration_beats` beat path) but launch
   *positions* land wherever the pad was hit. The pending-launch tick queue
   (`activate_due_pending_launches_at_tick`) compares against a frame counter
   masquerading as a MIDI tick — vestigial on this path. Either install the
   resolvers (snap from the beat clock) or route the quantize through the
   beat-stamp arm.
3. **Wrong-clock-epoch class inside the authority.** `sync_clips_to_time`
   starts clips with `realtime_now = Seconds::ZERO`, so their
   `recently_started_times` entry (0.0) never gates, the pending-pause
   deadline (0.1) is instantly expired, and the to-stop dirty-deadline is
   `max(deadline, 0.05)` — always in the past. `check_preparing_clips`
   re-anchors video clips correctly later, so the visible blast radius is
   small, but this is the exact trap `mark_compositor_dirty_now`'s comment
   documents, living in the core function. `last_realtime_now` is available;
   pass it.
4. **Serialized settings mutated per-frame outside the mutation gateway.**
   Authority auto-detect writes `project.settings.clock_authority` every tick
   (a manual `apply_authority_exclusively` is overwritten one frame later),
   and `sync_project_bpm_from_current_beat` writes `settings.bpm`. Neither
   bumps data_version or touches undo; a save mid-show persists whatever
   transient authority/BPM was live. Decide: either these are runtime-only
   (move them out of settings) or they are edits (route them).
5. **Live external tempo ignores authority.** Link enabled with peers (or CLK
   receiving) feeds `get_bpm_at_beat` and therefore *clip playback rates* even
   under Internal authority — deliberate for the BPM readout (comment says
   so), but rate math following a non-authoritative clock is a decision worth
   making explicitly.
6. **f32 seams in the beat plumbing.** The accumulator is f64, but external
   clock beats arrive via `Beats::from_f32(clk.current_clock_beat())`,
   beat-stamps are `f32`, `tick_audio_triggers`/`expire_due_oneshots` compare
   f32 beats, and TempoRecorder tracks f32. At beat ~25k (2h at 174 BPM) f32
   resolution is ~2ms of beat — inside the guard thresholds but eroding them.
7. **Hot-path allocations inside the tick**, against the stated discipline:
   `update_active_clip_playback_rates` / `correct_video_drift` /
   `seek_active_clips` / `pause_active_clips` / `resume_ready_clips` /
   `check_preparing_clips` / `process_pending_pauses` all clone id Vecs per
   call (several are per-frame), and `find_timeline_clip` is a full linear
   scan per active clip per frame (engine-side lookup cache exists in
   `Timeline` but needs `&mut`). At the canonical 53-layer / 2928-clip scale
   this is real per-frame work.
8. **`ActiveTimelineClipWindow` is dead code.** The incremental
   boundary-cursor query (built for exactly the big-project case) is never
   instantiated; the engine re-queries the full layer set each frame and
   `reset_active_clip_window()` is an empty stub. Wire it or delete it —
   today it's 680 lines of tested illusion.
9. **Tempo-map walks are linear and per-frame.** `seconds_to_beat_f64` runs
   on every `advance_time`; a recorded tempo lane (a point every 0.125 beats)
   from a long tempo-automated set makes the per-frame conversion O(points).
   No binary search, no segment cache.
10. **Live prewarm is a stub.** `append_live_prewarm_candidates` only returns
    already-active slots (TODO: MIDI-mapping + folder candidates);
    `LIVE_PREWARM_MAX_UNIQUE_CLIPS` / `RECENT_PRIORITY_COUNT` /
    `COMBINED_PREWARM_MAX` are unused. First MIDI fire of a cold video clip
    rides only the 0.02s recently-started gate — black-frame risk on stage.
11. **AbletonOSC inbound transport relay is disabled** (echo loops → play/pause
    oscillation; code TODO in `tick_sync_controllers`). In AbletonOSC mode,
    inbound transport currently depends on MIDI Clock being connected too.
12. **`suppress_next_transport` staleness.** The arbiter sets it on *any*
    gated play/pause; only senders consume it, and the Play/Pause command
    handlers clear it only when a sender is enabled. Sync-driven transport
    with senders disabled leaves the flag set; enabling SYNC later swallows
    the first real transport edge.
13. **`OscReceiver::subscribe_keyed`'s key scheme is broken by design** —
    `swap_remove` invalidates other keys for the same address. Harmless today
    (only `unsubscribe_all` is used, single-subscriber addresses) but a trap
    for the next subscriber.
14. **Round-trip constants are wall-clock guesses.** 0.3s seek cooldown and
    0.5s ownership grace encode a healthy localhost Ableton round trip; a
    loaded machine or networked DAW that exceeds them reintroduces the exact
    playhead drag-back they exist to stop. No measurement, no adaptivity.
15. **Session P2 seams (fresh code):** `session_launch_slot` from stopped
    transport calls `play()` (full sync — arrangement clips on that layer
    start) before the launch registers, then stops them one call later —
    a transient start/stop of a renderer inside one gesture. And
    `LiveClipManager` + `SessionRuntime` can both claim a layer (MIDI phantom
    on a session-overridden layer): live refs are still merged for a layer
    whose timeline query is suppressed — the layer plays the phantom over the
    session slot by scheduler-merge order, an interaction nobody has pinned
    down in a test yet.
16. **Device identity is positional.** `device_id = midir port index` and CLK
    source selection is by index; a device unplug/replug mid-show reorders
    ports — held NoteOff tracking (keyed by device_id) and the CLK source can
    silently attach to the wrong hardware.
