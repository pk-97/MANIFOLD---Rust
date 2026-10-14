# Frame-Exact Transport — one frame grid for live play and export

**Status:** IN PROGRESS · 2026-10-14 · P1 and P2 landed on main (ff922660e). P3 (a tempo-trim follower, a catch-up window, measured output latency, learned source offsets) is built on `feat/frame-exact-sync`. Owed: P3 review and landing, Peter's rig test (section 6 (Phasing), P3 demo), constants tuned from the rig log, P4. Bead: BUG-gqhep (live-transport-nondeterministic-frame-clock).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

<!-- index: Frame-exact transport clock (BUG-gqhep): impact map of every transport time/beat reader and writer, dt consumer, integrator, external sync source, export and harness; the integer frame-index clock shared with export on the song-time-0 grid, the content-tick grid, on-frame edge sampling, external sync as a tempo-trim clock follower (PI loop, trim cap, trim or seek, tempo-change catch-up window) over timestamped sources read at the screen instant with measured output latency and learned per-source offsets, latched clip start times for video, hand-back without a time jump, the MIDI clock tick filter with refit and the latched Link tempo guard, Ableton live-tempo following, phases P1–P4 and the fail-first proof plan. -->

Live play today moves the transport by measured wall time. Each frame samples whatever beat the jittered sum reached, so two plays of the same arrangement give different values on the same frame. Export does not have this problem: it computes time from a frame number. This design gives live play the same kind of clock. Time becomes a function of an integer frame index. Wall time only decides how many frames to step.

Peter's decision, 2026-10-13, quoted from the bead: "One frame-exact transport clock shared with export. Frame grid anchored at song time 0: frame_index N ↔ time N/project_fps; all consumers sample time(frame_index) computed absolutely, never accumulated; real dt only decides how many frames to advance (normally 1). Dropped frame → skip the index (stay in time). Link/MIDI clock/timecode corrections snap to whole frames (nudge the grid, never off-grid time); a correction shows as one held/skipped frame only when drift exceeds half a frame. Seek re-anchors to the grid; loops wrap the index; grid uses project fps, not display rate. Out of scope: audio-triggered clips (depend on live input)."

Peter's upgrades to P3, 2026-10-14, relayed by the lead. First, external sources correct phase with a small, slow, capped tempo trim, the standard clock follower, instead of held or skipped frames. Second, every external source is read at the moment the frame reaches the screen. They replace the "held/skipped frame" part of the 2026-10-13 ruling. The adversarial review of the first P3 revision and Peter's rulings on it, the same day, cut snaps for trim or seek, keep video steady through tempo changes, add a catch-up window for sudden tempo changes, keep time through a hand-back to Internal, and make output latency and source offsets measured rather than set. D6–D8 and D19–D22 carry them.

**For the instrument:** after P1+P2, an LFO, an automation lane or a clip cut lands on the same frame every time the same section plays under MANIFOLD's own clock. After P3, while Ableton drives, MANIFOLD's beat stays within about a millisecond of Ableton's (Link and MIDI clock) and moves smoothly: no held or skipped frames, no MIDI clock stair-steps. A tempo change in Ableton is followed on its frame under Link and within about a second otherwise, never with a jump back. Frames then repeat only as closely as Ableton's own timing does. Video frames and the water simulation are not covered until P4 and never, respectively (D16).

Companions:
- [CORE_ENGINE_MAP.md](CORE_ENGINE_MAP.md) — the current transport map; its section 4 (Time model & tempo) and section 7 (External sync) change when this lands.
- [VSYNC_AND_FRAME_PACING.md](VSYNC_AND_FRAME_PACING.md) — the content-thread timer this design re-anchors.
- [LIVE_SIM_CLOCK_DESIGN.md](LIVE_SIM_CLOCK_DESIGN.md) — the physics clock reads transport seconds and keeps its own wall-load cap.
- [ENCODE_REPLAY_DESIGN.md](ENCODE_REPLAY_DESIGN.md) — GPU dispatch caching; it never reads time (audit item 1.7).

## 1. Audit — what exists (verified 2026-10-13 on 09a44cb4c)

Extend, don't redesign. Every anchor below was read on this base. Re-run the commands in each phase brief before trusting a line number.

### 1.1 How live time moves today

| Finding | Where | Verified state |
|---|---|---|
| The content timer returns wall dt | `crates/manifold-app/src/frame_timer.rs:360-386` (`consume_tick_at`) | True. `frame_clocked` returns the target dt instead; only harnesses set it (`frame_timer.rs:413-416`, feature `perf-soak` or test). |
| The deadline is not phase-anchored | `frame_timer.rs:161-170`, `:367` | The next deadline is `last_tick_time + period`, and `last_tick_time` is reset to "now" at each consume. Every bit of lateness is kept forever, so the tick rate runs slightly under project fps. |
| The content thread passes wall dt to the engine | `crates/manifold-app/src/content_thread.rs:574-578`, `:646-653` | True. `tick_frame` calls `consume_tick()` with no injectable instant. `dt_seconds` and `pre_render_dt` both carry wall dt. |
| The engine accumulates | `crates/manifold-playback/src/engine.rs:787-792` (`advance_time`), called at `:1010` | True: `current_time_double += dt × playback_speed`, then `update_beat_from_time` (`:2133-2141`). |
| Playback speed | `engine.rs:733-735` | `set_playback_speed` has no caller. Speed is always 1.0. |
| Automation samples the beat with no grid | `engine.rs:1054-1067`; `crates/manifold-playback/src/automation.rs:360` | True: `lane.value_at(current_beat)`. |
| External nudges | `engine.rs:796` | The inherent `nudge_time` is called only by a unit test (`engine.rs:3725`). Production nudges reach `SyncArbiterTarget::nudge_time` (`engine.rs:3326`), which calls `set_time`. |
| Export is frame-exact in time | `engine.rs:1001-1008` | True: `set_time(origin + frame_count × export_fixed_dt)`. Origin is the export start, not song time 0. But see the on-frame edge finding (audit item 1.4) and export ordering (audit item 1.7). |
| Export freezes under external sync | `engine.rs:1000-1010`; `set_export_mode` `engine.rs:745-774` | New finding. The export branch sits inside `if !self.external_time_sync`. `set_export_mode` never clears that flag, and nothing in `content_export.rs` does. An export started while CLK or timecode is driving renders the start frame for every frame. |

The Play press has no phase anchor. `ContentCommand::Play` (`crates/manifold-app/src/content_commands.rs:880-913`) flips the state. The next tick then adds whatever wall time passed since the previous tick, which depends on where between two ticks Play was pressed.

### 1.2 Readers and writers of transport time and beat

Classes: **GRID** = must sample the frame grid. **WALL** = must keep real wall time. **DERIVED** = reads a value already computed from the transport, so it changes for free. **EXT** = an external source moving the transport; becomes a correction.

Writers (each is replaced or rerouted by this design):

| Writer | Where | Class | After |
|---|---|---|---|
| Live advance | `engine.rs:1010` → `:787` | GRID | Deleted. The clock steps its index. |
| Export advance | `engine.rs:1001-1008` | GRID | The same clock in export mode (D9). |
| `set_time` | `engine.rs:692-697`; callers `content_commands.rs:909` (Play aligns to MIDI clock), `engine.rs:3326` (trait nudge), tests | GRID | `locate` (D4); trait nudge becomes a correction (P3). |
| `seek_to` | `engine.rs:803-853`; callers `content_commands.rs:949`, `:963`; `crates/manifold-playback/src/transport_controller.rs:184`, `:196`; `content_export.rs:427`, `:497`, `:717`, `:789`; arbiter `engine.rs:3330` | GRID | Snaps forward to the grid (D4). |
| `set_beat` + `sync_time_from_beat` | `engine.rs:699-717`; callers `content_thread.rs:1938-1940` (Link), `:1959-1961` (MIDI clock), `content_commands.rs:907` (Play) | EXT | P2 stopgap `follow_external_beat`; P3 correction targets. |
| `set_beat` after a tempo-map change | `content_thread.rs:2110-2111` | DERIVED | `rederive_beat()`: beat from clock time. |
| Tempo edit reconcile | `engine.rs:721-731`; callers `content_commands.rs:2218`, `transport_controller.rs:279` | GRID | D11. |
| Stop / initialize | `engine.rs:675-677`, `:579-581` | GRID | Index 0. |
| `set_external_time_sync` | `crates/manifold-playback/src/midi_clock_sync.rs:591`, `crates/manifold-playback/src/osc_sync.rs:373`, `transport_controller.rs:103` | EXT | Kept as the P2 stopgap; deleted in P3 (D6). |

Readers inside the engine:

| Reader | Where | Class |
|---|---|---|
| `sync_clips_to_time` membership, backward-move guard | `engine.rs:1625-1740` | DERIVED (the backward guard at `:1633-1642` matters for D6) |
| Clip window query | `engine.rs:1301-1367` → `crates/manifold-core/src/timeline.rs:426-445` → `crates/manifold-core/src/layer.rs:526-570` | DERIVED; edge compare `start_beat <= through`, `end_beat > from` (audit item 1.4) |
| Automation (play / inspect / previews) | `engine.rs:1054-1078`, `:1178-1201` | DERIVED |
| Modulation: drivers, envelopes, trigger stamps | `engine.rs:1095-1108`, `:1232-1248`, `:933-941` | DERIVED |
| Session launch, stop, quantize, seek retarget | `engine.rs:816`, `:1854-1980` | DERIVED |
| One-shot expiry | `engine.rs:2296-2301` | DERIVED |
| Video rate, expected source time, drift cadence | `engine.rs:2494-2530`, `:2746-2866`, `:1125-1130` | DERIVED |
| `pre_render` for every renderer | `engine.rs:2961-2968` | DERIVED time; dt per audit item 1.3 |
| Project BPM display write | `engine.rs:2362-2384` | DERIVED |
| Beat↔time helpers | `engine.rs:2133-2170`, `:3266`; 13 direct `TempoMapConverter::` calls in `engine.rs` | DERIVED; all route through one converter in P3 (D8) |
| `SyncTarget::current_time`, `LiveClipHost::current_beat` | `engine.rs:3175-3178`, `:3284` | DERIVED |

Readers outside the engine:

| Reader | Where | Class |
|---|---|---|
| Generator and compositor frame time/beat | `crates/manifold-app/src/content_pipeline.rs:2235-2236` → `:2524-2527`, `:2801-2806`; clip atlas `:3189` | DERIVED |
| Node-graph frame clock | `crates/manifold-node-engine/src/runtime/core.rs:1681-1688`, `:1942-1948` → `FrameTime` | DERIVED |
| Primitives sampling absolute time or beat | `lfo.rs:170`, `:180`; `beat_ramp.rs:86`; `trigger_ease_to.rs:201`; `terminal_stream.rs:162`; `terminal_reaction.rs:171-177`; `flow_field_noise.rs:143`; `scanline_jitter_field.rs:197`; `track_regions.rs:478` (nodes-image); `loop_camera.rs:294`; `glitch_jitter.rs:140`; `noise_displace.rs:139`; `ripple_mesh.rs:158`; `ocean_spectrum.rs:124`; `render_lines.rs:533`; `gltf_animation_source.rs:339`, `:409`; `gltf_skeleton_pose.rs:552`, `:632`; `gltf_morph_weights.rs:275`, `:346` (nodes-scene) | DERIVED |
| Physics and liquid clocks | `crates/manifold-water-rigid/src/primitives/physics_world.rs:467`; `crates/manifold-water-gpu-mpm/src/primitives/matter_domain.rs:590-953`; `crates/manifold-water-surface/src/primitives/liquid_frame.rs:209` → `crates/manifold-physics/src/clock.rs:278` | DERIVED time, but wall-load capped (D16) |
| MIDI note stamps, one-shots, recording | `crates/manifold-playback/src/live_clip_manager.rs:425-576`, `:959`, `:1011` | DERIVED |
| Clip-launch callbacks | `crates/manifold-playback/src/clip_launcher.rs:181`, `:268`, `:405`, `:516` | DERIVED (report only) |
| Audio-layer voices | `crates/manifold-playback/src/audio_layer_playback.rs:324` | DERIVED beat; the voice keeps the audio clock (WALL) |
| Outbound Ableton / M4L beat | `content_thread.rs:670-675`, `:691-697` | DERIVED beat; retransmit windows are WALL |
| Tempo recorder stamps | `content_thread.rs:2056-2081` | DERIVED |
| Link offset cache | `content_thread.rs:1970-1987` | EXT (audit item 1.6) |
| External position compares | `midi_clock_sync.rs:777-797`, `osc_sync.rs:410-428`, `crates/manifold-playback/src/transport_sync.rs:31` | EXT |
| Seek echo to Ableton | `content_commands.rs:952`, `:965` | DERIVED |
| `FireParameter` frame stamp | `content_commands.rs:1147-1152` | DERIVED |
| Audio analysis epoch | `crates/manifold-app/src/audio_mod_runtime.rs:337-345` (`transport_epoch`) | DERIVED; corrections must not bump the epoch (D6) |
| UI playhead, transport bar, perform HUD, autosave | `crates/manifold-app/src/content_state.rs:119-121` → `ui_bridge/state_sync.rs:435-436`, `ui_bridge/projection/transport.rs:123`, `perform_mode/render.rs:97`, `autosave.rs:268` | DERIVED. No interpolation on the playhead path. |
| Harness readers | `perf_soak.rs:558`, `:983`; `gap_start_probe.rs:94`, `:105`; `scene_modifier_journey.rs:483`; `scene_modifier_journey/periodic.rs:231`; `scene_modifier_journey/angular.rs:210`; `corridor_acceptance.rs:388`; `gpu_flip_export_demo.rs:197`, `:202` | DERIVED (audit item 1.8) |

LED output reads no time (`rg '\bbeat\b|\btime\b|\bdt\b|Instant' crates/manifold-led/src` has no hits outside tests).

Re-derive: `rg -n '\.(current_time_double|current_time|current_beat|current_beat_f64|time_to_timeline_beat|beat_to_timeline_time|advance_time|nudge_time|set_time|set_beat|sync_time_from_beat|seek_to|set_export_origin|transport_epoch)\(' crates -g '*.rs'`. On this base it hits 28 files outside `engine.rs`. The four not in the tables: `input_handler.rs` (UI seeks, including the End key's `f64::MAX` sentinel, which `input_host.rs:489` resolves to the song end before it reaches the engine), `ui_bridge/inspector.rs:2513` (a test), `crates/manifold-media/src/decode_scheduler.rs:317` (a decoder seek, not the transport) and `content_pipeline/trigger_lane_force_tests.rs` (reads only).

### 1.3 Where dt flows past the transport

Today one wall dt feeds everything below. After the change the **tick dt** is `frames × step`: the content-tick frames since the last tick times one project frame (D5).

| Consumer | Where | Uses today | After | Deterministic per frame index? |
|---|---|---|---|---|
| Generators (`PresetContext.dt`) | `content_pipeline.rs:2527` → `crates/manifold-compositor/src/generator_renderer.rs:911-925` | wall dt | tick dt | Yes for samplers. Integrators: see the next rows. |
| Compositor / effect chains (`CompositorFrame.dt`) | `content_pipeline.rs:2804` | wall dt | tick dt | Same. |
| Linear time sums | `inject_burst.rs:195`, `:306` (`elapsed += dt`); `track_regions.rs:181-182` (`age`, `unmatched_seconds += dt`) (nodes-image) | `ctx.time.delta` | tick dt | Bit-equal on runs with the same drop pattern. A 2-frame tick adds `2 × step` where two ticks add `step` twice: equal in real arithmetic, not always in bits. |
| Exponential decay | `envelope_decay.rs:160` (`env *= exp(−rate × dt)`) | `ctx.time.delta` | tick dt | Same as linear sums. |
| Input-driven one-pole filters | `envelope_follower_ar.rs:199`; `smoothing.rs:142`, `one_euro_filter.rs:126`, `compressor_envelope.rs:172` (nodes-image); `nested_cubes_geometry.rs:226`, `render_lines.rs:534` (nodes-scene); particle `frame_delta = delta × 60` at `crates/manifold-node-engine/src/scene/boundary_nodes.rs:402` | wall dt, also while paused | tick dt, also while paused | Only on drop-free runs. A dropped frame skips an input sample. |
| Beat-delta sum | `terminal_reaction.rs:171-177` (`clock += (beat − previous).clamp(0, 8)`) | beat, not dt | unchanged | Yes. The sum telescopes to the absolute beat, except across a seek of 8 beats or more. No tick-dt change. |
| Temporal reset detector | `crates/manifold-node-engine/src/exec/temporal_reset.rs:63-76`; `render_mesh_diagram.rs:322-336` | resets when `| |Δseconds| − delta | > 1.5 × delta` | unchanged rule | While playing, Δseconds equals tick dt, so no reset. A hold (Δ 0) and a one-frame skip (Δ 2 frames) stay inside the rule. A net move of 3 frames or more, from a jump or a seek, resets feedback and trails on that frame. Today's nudges do the same. |
| Modulation audio followers | `crates/manifold-playback/src/modulation.rs:378`, `:930-935` | wall dt | tick dt | Live audio input; outside the claim. |
| Audio clip triggers | `engine.rs:1024`, `:2272-2290` | wall dt | tick dt | Live input; out of scope per Peter. |
| Video decode pacing | `crates/manifold-media/src/video_renderer.rs:812-813` | two integrators: `time_accumulator += dt × rate` and `playback_time += dt × rate`; the 3-frame-behind seek targets `playback_time` (`:829`) | P4: the target frame from transport time | No until P4. Drift correction re-seeks only every 2 s when 0.1 s off (`engine.rs:1125-1130`, `:2746`). |
| Physics / liquid sim | `crates/manifold-physics/src/clock.rs:278-400` via transport seconds | transport seconds plus wall load | unchanged code | **No** (D16). `live_cap_for` (`clock.rs:125-130`) drops the cap to one interval after a frame over the wall budget, and `clock.rs:380` reanchors when intervals are discarded. Both depend on machine load. |
| Sim late-frame rule | `content_thread.rs:790-803`, `:425-431` | wall ms | unchanged (WALL) | Must stay wall: it measures load. |
| MIDI NoteOff 5 ms guard | `live_clip_manager.rs:40`, `:761` | wall `realtime_now` | unchanged (WALL) | A human-input debounce. |
| Compositor dirty deadlines, recently-started gate, pending pauses, prewarm cadence | `engine.rs:850`, `:1484-1530`, `:2921-2942`, `:3026-3050` | wall `realtime_now` | unchanged (WALL) | Decode-latency guards. |
| Sync controllers, arbiter cooldowns, Ableton retransmit | `content_thread.rs:1694-1913`; `sync.rs`; `crates/manifold-playback/src/ableton_bridge.rs:2404-2429`; `crates/manifold-playback/src/osc_sender.rs:113-137` | wall `time_since_start` | unchanged (WALL) | Round-trip timing. |
| Percussion status timer | `content_thread.rs:709-717` | wall | unchanged (WALL) | UI status only. |
| Audio-layer voices | `audio_layer_playback.rs:51-55` | own audio clock; reseek over 200 ms | unchanged | A frame never reaches 200 ms. |
| UI thread animation | `crates/manifold-app/src/app_render.rs:220-222` (its own `FrameTimer`, `app.rs:702`) | wall | unchanged (WALL) | D2 must not change it (`app.rs:3135-3143` calls `should_tick`). |
| `frame_count` handed to graphs | effects: content tick count (`content_pipeline.rs:2806`); generators: always 0 (`generator_renderer.rs:922`) | tick counter | unchanged | Deferred. |

### 1.4 Tempo and on-frame edges

Time is the integration axis and beats are derived, in live play and export alike: `update_beat_from_time` → `TempoMapConverter::seconds_to_beat_f64` (`engine.rs:2133-2141`, `crates/manifold-core/src/tempo.rs:339-408`). The map is piecewise constant BPM in f64. Every loaded project has a beat-0 tempo point (`crates/manifold-core/src/project/load_migration.rs:75`), so `settings.bpm` is never the conversion tempo for a loaded project.

**On-frame edges round below their beat.** The clip window keeps a clip only when `start_beat <= current_beat` (`layer.rs:539`, `:578`). `current_beat = (k × (1/fps)) / (60/bpm)` in f64 lands one ulp below an edge that sits exactly on frame k often enough to matter. The clip then shows one frame late, and the outgoing clip one frame long. Scripted over every 1/16-beat edge exactly on a frame in the first 2 hours, with an exact rational on-frame test:

| fps | share of on-frame edges below their beat, 120–180 BPM | worst cases |
|---|---|---|
| 60 | 10% | 150 BPM 40%, 174 BPM 25% |
| 30 | 7% | 150 BPM 40%, 174 BPM 26% |
| 24 | 5% | 174 BPM 25%, 180 BPM 16% |
| 25 / 50 | 3% | 150 BPM 17%, 174 BPM 26% |
| f32 23.976 / 29.97 / 59.94 | 0% | NTSC rates put almost no edge exactly on a frame |

The error is at most 3.6e-12 beats. Today's export uses the same formula, so exports at these tempos cut one frame late on those edges. The same rounding reaches anything that compares the beat with a stored edge: automation step segments, session quantise targets and one-shot expiry. `Driver::evaluate_frame_aligned` already carries a 1e-7-frame tolerance for this class (`crates/manifold-core/src/effects/driver.rs:189-200`). D14 applies it once at the source.

Two writers change the map or BPM under a playing transport:
- User tempo edits keep the beat and move time (`engine.rs:721-731`). D11 snaps it.
- `apply_resolved_tempo` writes a live external tempo into the beat-0 point under Internal authority (`content_thread.rs:2083-2103`), then keeps time and moves the beat (`:2110-2111`). Live tempo is only cleared when there is no project (`content_thread.rs:2033-2036`), so a stale Link tempo can stay after peers vanish. Pre-existing; P3 fixes it (D21).

### 1.5 Project fps, display rate, content tick rate

- The content timer runs at `settings.frame_rate` (`f32`, `crates/manifold-core/src/settings.rs:26`). Four sites copy it into the timer: load (`content_commands.rs:1491-1493`), `SetFrameRate` (`:1653-1657`, which also writes the setting outside `EditingService`) and undo/redo (`:1357-1359`, `:1435-1437`). rt-capture overrides the timer after load on purpose (`crates/manifold-app/src/rt_capture.rs:483`, `:498`). Copies are how two rates diverge; D10 removes them.
- The display rate never drives the content thread. At 120 Hz each 60 fps content frame is already shown twice. Nothing here changes that.
- Non-integer fps: f32 29.97 is 1.02e-6 slower than 30000/1001, 7.4 ms over 2 hours. Live and export use the same f32 rate. Export stamps the container with the exact rational (`crates/manifold-media/src/frame_rate.rs:40-61`, BUG-129 (export NTSC timebase drift)). No rational type is needed for the transport.
- Float error: `index as f64 × step` differs from the exact `index / fps` by at most 9e-13 s at frame 432,000, for 60, 24 and f32 29.97. Computed.

### 1.6 External sync today

Authority is chosen every frame: MIDI clock receiving, then OSC receiving, then Link with peers, then Internal (`content_thread.rs:1706-1743`).

| Source | How it moves the transport today | Finding |
|---|---|---|
| MIDI clock | While receiving and playing, `external_time_sync = true` stops the engine advancing (`midi_clock_sync.rs:585-596`, `engine.rs:1000`). Each frame sets the beat to the last clock tick (`content_thread.rs:1943-1962`). The beat is `(sixteenths + tick/6)/4` (`midi_clock_sync.rs:368-370`). | **Stair-steps today.** One tick is 20.8 ms at 120 BPM, longer than a frame. The midir callback discards the arrival timestamp (`_timestamp_us`, `midi_clock_sync.rs:134`). State is one packed `AtomicU64` with 14 spare bits (`:19-58`), too few for a timestamp. Nudge under 2 s, seek otherwise (`:777-797`); the 2 s exists to absorb tempo ramps (`:781-786`), so ramps are routine. |
| Link | Never sets `external_time_sync`. Each frame, when MANIFOLD does not own playback and the offset is valid, the beat becomes `link_beat − offset` (`content_thread.rs:1928-1941`). The engine then adds a wall dt on top. | `cache_link_beat_offset` (`content_thread.rs:1973-1987`) uses `link.current_beat` from the previous poll against the engine's current time, so the offset carries up to one poll of error. It runs only on MANIFOLD's own Play/Seek (`content_commands.rs:912`, `:958`, `:970`). Stop and load poison it with NaN (`:936`, `:1470`). A Link-initiated Play (`crates/manifold-playback/src/link_sync.rs:150-152`) never caches it, so after any Stop Link never steers position again until MANIFOLD seeks. No quantum alignment. |
| OSC timecode (M4L) | Receiving sets `external_time_sync` (`osc_sync.rs:368-373`). Each message nudges under 0.5 s, seeks otherwise (`osc_sync.rs:405-428`). | 25–30 Hz messages, each quantised to its own timecode frame: about ½ timecode frame of average lag. |
| AbletonOSC relay | Play / Pause / SeekBeats / NudgeBeats at listener cadence, CLK-gated (`content_thread.rs:1858-1912`). Nudge under 2 s, seek otherwise (`NUDGE_THRESHOLD_SECS`, `transport_sync.rs:31`). | The relay already estimates round-trip time (`transport_sync.rs:34-37`). Its position lags by network latency. |

These sources are not white noise. A 24 PPQN clock against a 60 fps grid, and 25–30 Hz timecode against 60 fps, beat against each other. The sampling error is periodic, and an average of a periodic error keeps a bias that depends on the tempo. D7 removes the error at the source, with arrival timestamps and projection to the frame deadline.

### 1.7 Export and encode replay

- Export: `seek_to(start)` and `play()` (`content_export.rs:714-718`), optional warm-up ticks with an accumulating clock (`:758-786`), a re-seek (`:789`), then `set_export_origin(start)` (`:796`) and frames `0..total` with `export_fixed_dt = 1.0 / fps as f64` (`:566-574`, `:812-852`). Frame 0 renders the start with dt 0 (`:960-973`). Export fps comes from `ExportConfig.fps: f32` (`crates/manifold-media/src/export_config.rs:13`) and may differ from project fps.
- **Ordering.** Both seeks run before the export origin is set. On the new clock they would snap on the live grid at project fps, not the export grid. A clip starting between the export start and the next live-grid frame would then be started by the seek, before export frame 0. D9 sets the export clock first.
- The origin is the export start. When that start is off the song-time-0 grid, live and export sample different times for the same song moment. At 128 BPM a bar is 112.5 frames at 60 fps, so every odd bar starts off grid. Ruled by D17: export moves to the song-time-0 grid.
- Encode replay never reads time (`rg -n -i '\bbeats?\b|transport|current_time|frame_time|Seconds' crates/manifold-gpu/src/replay.rs crates/manifold-gpu/src/metal/replay.rs crates/manifold-gpu/src/vulkan/replay.rs` has no hits). Outside this design.

### 1.8 Harnesses and tests that lean on today's behaviour

- `frame_clocked`: 26 matches, 25 calls plus the definition (`rg -c 'set_frame_clocked\(' crates`). Rechecked against D4. Every harness seeks while stopped or paused, then plays: rt-dynamic held-out (`rt_dynamic_held_out.rs:327-328`), blob and scene-modifier performance, the journeys, corridor acceptance (`corridor_acceptance.rs:657`, `:907`), gap start (`gap_start_probe.rs:71`, `:98`), rt-capture and `frame_time.rs:845`. `mute_visibility_probe.rs` (2 calls) never seeks. None seeks while playing, so the D4 hold never fires in a harness. Play advances one frame from the anchor on the first tick, as `frame_clocked` does today. Only the forward snap of a paused seek changes what they see:
  - `scene_modifier_journey/angular.rs:207-211` breaks. It seeks to beats 0.99999 and 1.00001 to straddle a sawtooth reset and asserts `|actual − beat| < 1e-6`. Both snap, and 0.99999 lands on 1.0, after the reset. P2 moves the probe to the grid frames either side of beat 1 (`journey-proofs` feature).
  - `angular.rs:259` (1.00001), `consolidation.rs:93`, `:170` (0.1), `periodic.rs:229`, `:289` and `scene_modifier_journey.rs:561`, `:608` (4.25) compare captures taken at the same snapped beat, or compute expectations from `current_beat_f64()`. They keep working. P2 reruns them once.
  - `gap_start_probe.rs:105` makes its own `beat >= start_beat` compare. D14 makes the engine's beat land at or past on-frame edges, so the probe and the engine agree.
- `manifold frame-time` (`crates/manifold-app/src/frame_time.rs:1-33`) measures production pacing headless. It is the oracle for the timer change.
- `headless_content_thread` opens the test GPU device (`crates/manifold-app/src/headless_harness.rs:26`). App-level tests built on it are single queued GPU runs, not CPU proofs.
- `TickContext` struct literals: 33 matches (`rg -n 'TickContext \{' crates -g '*.rs'`): `engine.rs` 3 (the definition and 2 test literals), `tests/engine_tick.rs` 15, `tests/param_step_clip_edge.rs` 3, `tests/sync_engine_integration.rs` 2, `tests/session_mode.rs` 1, `tests/trigger_source_switch.rs` 1, `content_thread.rs` 1, `content_export.rs` 2, `content_pipeline/trigger_lane_force_tests.rs` 2, `bug219_verify.rs` 1, `import_responsiveness_verify.rs` 1, `ui_snapshot/script.rs` 1. Plus `TickContext::default()` in engine unit tests.
- Tests that step by non-frame amounts: `tests/param_step_clip_edge.rs` uses 0.25, 0.295, 0.005, 0.5, 0.1, 0.03, 0.01 and 1000.0 s (`:96-107`, `:200-219`). The mapping is in the P2 seam brief.
- Tests that assume accumulation within a tolerance: `tests/engine_tick.rs:422-458`. They pass unchanged on the grid.
- Timer tests: `frame_timer.rs:518-531`, `:550-569`, `:636-647`.
- `set_state(PlaybackState::Playing)` bypasses `play()`; all 37 callers are tests.

### 1.9 Dead and stale things found on the way

- Inherent `PlaybackEngine::nudge_time` (`engine.rs:796`): test-only caller. Deleted in P2.
- `on_time_changed`, `beat_snapped_beat_resolver`, `absolute_tick_resolver`, `replenish_warm_cache`: never installed (`engine.rs:369-375`). `on_time_changed` is deleted with the advance block in P2.
- `playback_speed` (`engine.rs:119`, `:422`, `:733`): no writer. Deleted in P2.
- CORE_ENGINE_MAP.md section 13 (Honest edges), item 2, names `activate_due_pending_launches_at_tick`, which no longer exists. Fixed in the landing sweep.
- CORE_ENGINE_MAP.md section 3 (The frame, end to end) names the inherent `nudge_time` as the nudge path. Fixed in the landing sweep.

### 1.10 External sync re-read for P3 (verified 2026-10-14 on f32e3c5fd)

Audit items 1.1–1.9 cite the pre-P2 base. These anchors are current. P3's work list: `rg -n 'STOPGAP\(frame-exact P3\)' crates` gives 23 markers in 9 files; `rg -l 'external_time_sync|follow_external_beat' crates` gives 9 files, including `tests/trigger_source_switch.rs` (16 hits) and `tests/osc_timecode.rs:73-88`.

| Finding | Where | Consequence for P3 |
|---|---|---|
| The P3 types exist but nothing calls them: `PhaseLock`, `Correction`, `Smoothing`, `observe_external`, `clear_external`, `LiveTempo` | `transport_clock.rs:22-46`, `:82-207`, `:416-438`, `:440-479`; no caller outside the module and its tests | Replacing them costs nothing downstream (D6). |
| The stopgap shows the source one frame ahead: it compares `target + step` with the frame after the advance | `engine.rs:1182-1188` | Pre-P2 place-then-advance did the same. It hides about one frame of pipeline delay. D19. |
| While CLK or timecode is authority, the clock does not advance and every tick re-places it | `engine.rs:140-142`, `:1175-1192`; flag set at `midi_clock_sync.rs:586-597`, `osc_sync.rs:371-373` | Deleted (D6). |
| Two writers move the position under CLK: the controller nudges through the arbiter, and the content thread copies the last tick's beat every frame | `midi_clock_sync.rs:779-796`; `content_thread.rs:1989-2009` | One sample path per source (D7). |
| The midir callback drops the arrival timestamp | `midi_clock_sync.rs:134` | D20. |
| Every 0xF8 advances the position, including the first after Start or Continue, and ticks count while stopped | `midi_clock_sync.rs:143-171` | MIDI 1.0 says playback starts on the first clock after Start, so that clock is the start position. Counting it puts the beat one tick (1/24 beat, 20.8 ms at 120 BPM) early. ⚠ VERIFY-AT-IMPL on Peter's rig: log raw bytes from Live around a Start. D20. |
| The CLK BPM is polled once per frame with `f32` seconds, over a 96-tick window, then an EMA with α = 0.3 | `midi_clock_sync.rs:281-283`, `:321-324`, `:578-584`, `:669-733` | It reacts in about 2 s at 120 BPM, and `f32` seconds lose resolution over a set (about 0.5 ms at 2 hours). Replaced (D20). |
| Link is already read at the tick's deadline. Its tempo is the session tempo | `link_sync.rs:95-114`; `content_thread.rs:589`, `:1785` | Kept; the read instant gains the output latency (D19). |
| Link offset: from the last poll against the engine's time, cached only on MANIFOLD's Play and Seek, NaN after Stop and load. A Link-initiated play never caches it | `content_thread.rs:2020-2034`; `content_commands.rs:913`, `:937`, `:959`, `:971`, `:1425`; `link_sync.rs:153-155` | BUG-0u1al (Link-initiated Play never caches the Link beat offset). D15. |
| Live tempo is set only while Link has peers or CLK receives, and is cleared only when there is no project. Under Internal it can only be stale, and `apply_resolved_tempo` writes it into the beat-0 point | `content_thread.rs:1784-1828`, `:2080-2083`, `:2131-2151` | BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point). D21. |
| Link tempo already outranks the CLK estimate as the live tempo | `content_thread.rs:1819-1827` | ABLETON_TRANSPORT_SYNC_DESIGN.md calls Link the tempo plane (its lines 59-63). D21 keeps that for the follower's base tempo. |
| AbletonOSC values are caught on the receive thread into a latest-only slot and stamped with the frame's time when drained. Several reports in one frame collapse to one | `ableton_bridge.rs:641-662`, `:907-910`, `:2458-2459` | Up to one frame of stamp error: about 8 ms of mean lag in the D7 noise model. D7. |
| The AbletonOSC relay has a tempo listener, used only for dead reckoning | `transport_sync.rs:326-331` | D8's "no live tempo channel" was wrong. D8 now follows it. |
| The timecode sender is LiveMTC, a third-party Max for Live device, not in this repo. Peter ruled the M4L path unused on 2026-07-07 | `osc_sync.rs:4`, `:14-15`; ABLETON_TRANSPORT_SYNC_DESIGN.md lines 65-67 and 476-479 | P3 cannot change the sender (D7, and the Deferred table). |
| The timecode receiver takes one float of song seconds, but every OSC argument becomes `f32` | `osc_sync.rs:314`; `osc_receiver.rs:197-200` | `f32` seconds are 0.24 ms coarse at one hour. D7 keeps doubles. |
| Timecode and OSC messages are stamped when the frame drains them, not when the packet arrives | `osc_receiver.rs:161-186`; `content_thread.rs:1877` | D7 stamps on the receive thread. |
| The backward-move guard is beat-based | `engine.rs:1815-1824` | Only a backward seek reaches it after P3; the trim never moves the beat backward (D6). |
| No output-latency setting exists | `rg -i 'output.?latency|display.?latency|av.?offset' crates` has no hits | D19. |
| Machine preferences live in `UserPrefs`, a JSON key-value file outside any project | `user_prefs.rs:1-16`, `:62-164`; keys like `MANIFOLD_BlenderPath` (`blender_import.rs:32`) | D19 stores the hidden extra delay and the learned source offsets there. |
| The stage output window is presented from the content thread, in the frame's own command buffer | `content_pipeline.rs:3332-3362`; `GpuEncoder::present_drawable` (`manifold-gpu` `surface.rs:359`) | D19 measures the present delay there with no cross-thread pairing. A per-frame Metal handler already exists as a precedent (`encoder.rs:2695`). |
| The UI bridge already tags each published frame with its content frame number | `shared_texture.rs:305`, `:331`; used at `editor_bridge.rs:1618` | Available if the editor window ever needs its own present measurement (section 9 (Deferred)). |
| macOS reports no display latency here | `CVDisplayLinkGetOutputVideoLatency` on this Mac's built-in panel returns 0/0 with no valid flags; `ioreg -l -w0` shows `DisplayAttributes` and `ProductAttributes` but no raw `EDID` key (scratchpad probe, 2026-10-14) | D19 reads both on the rig and assumes nothing. |
| Every beat→time conversion goes through the map, and video elapsed time is `time − map(start_beat)` for non-warped clips | `engine.rs:2353-2359`, `:2648-2676`, `:2723-2731`; the recently-started gate inlines the map (`:3188-3198`); 13 callers across `engine.rs`, `live_clip_manager.rs`, `transport_controller.rs`, `content_commands.rs`, `content_thread.rs`, `content_export.rs` | Through the follow segment these would rescale whole elapsed times on every tempo change. D8 latches clip starts and computes durations from beat differences. |
| MIDI notes reach the content thread through a `std::sync::mpsc` channel from the midir callback | `midi_input.rs:115`, `:470` | D20 reuses that shape for clock events. |
| No clock-follower code exists in the repo | `rg -i 'delay.locked|\bDLL\b|phase.locked|\bPLL\b|clock follower' crates scripts tools` finds nothing relevant | The follower is new (`ExternalFollower`, D6). |

The Link-offset bead has a duplicate from the playback audit pilot: `bd show BUG-mmct0.9`. Close both at landing.

## 2. Decisions

**D1 — One clock type, integer index, absolute time.** `TransportClock` holds an `i64` frame index and a step. Time is always `index as f64 × step`, with `step = 1.0 / fps as f64` from the `f32` fps. Live and export both count from song time 0 (D17), so there is no origin. Nothing ever adds to time. The expression is today's export expression with the origin at 0.
Rejected: an exact rational `(num, den)` type. Its benefit is 7.4 ms over 2 hours at 29.97. Rejected: a time accumulator snapped at read time. It reintroduces the path dependence this bead removes.

**D2 — The content tick runs on an anchored grid, counted by instant comparison.** Deadline n is `anchor + Duration::from_secs_f64(n as f64 × step)`. The timer counts `frames` as the number of deadlines with `deadline(n) <= now` since the last tick. It decides by comparing `Instant`s, never by dividing elapsed time. After a long stall it estimates n from elapsed time only to bound the search. The comparison against `deadline(n)` and `deadline(n + 1)` decides. Normally `frames` is 1. The anchor resets on load and on an fps change. With `frame_clocked`, `frames` is always 1. The UI thread's timer keeps today's free-running deadline.
The counting lives in `FrameGrid` (`crates/manifold-playback/src/frame_grid.rs`), which holds the `Instant`s so the clock stays pure. The content thread's `FrameTimer` wraps it (section 3.3). `FrameGrid` and the clock share `step_for_fps` and the fps rule (D10), so their steps agree bit for bit. Deadline indices stop at 2^32, so no deadline can overflow an `Instant`.
Why anchored: with a free-running deadline every late microsecond is kept, and counting grid frames against it skips a transport frame about once a minute at 5 µs lateness per tick. Computed. With anchored deadlines, sub-frame jitter never changes `frames`.
Why compare instants: a division near a boundary can disagree with the timer's own deadline by one ulp, and the timer would then wait for a deadline that the count already passed.

**D3 — Stalls skip and stay in time.** After a stall of any length, the transport skips the deadlines passed. No cap. The 5 s GPU surface timeout (`content_thread.rs:485`, `:498-502`) is the longest stall the content thread itself creates. The physics clock keeps its own rule, with at most two Sim Rate intervals per frame and the rest discarded (LIVE_SIM_CLOCK_DESIGN.md section 7 (Decided — do not reopen)). The transport keeps time while the water goes slow-motion.

**D4 — Re-anchors snap to the grid. Only a seek while playing holds.**
- Snap rule: `index_at_or_after(t) = ceil(t / step − 2e-7)`. The tolerance is twice D14's sample offset, so a beat sampled on frame k, turned back into a time, snaps to k: with equal tolerances about one export restore in ten landed a frame late. A scripted round trip at nine rates over frames 0–5,000 and 431,000–433,000 had zero mismatches. Cost: an edge between 1e-7 and 2e-7 of a frame after frame k snaps to k, where its beat shows on k + 1. No 1/16 edge at a whole-number BPM and fps falls there.
- Play and resume: anchor at or after the current time and advance normally. The first playing tick moves `frames` from the anchor, exactly as a `frame_clocked` harness does today. The paused frame was already on screen, so nothing repeats.
- Seek while stopped or paused: snap forward. The next tick renders it because the transport is not moving.
- Seek while playing: snap forward and hold one advance, so the next tick renders the target frame. Without the hold the first frame after a seek to a clip start would be one frame into the clip.
- Under external authority (P3), a seek (the source's own, or the follower's after a lasting error) goes to the frame nearest the target's map time, with no hold, and the follower re-seeds there at the source's beat (D6). A hold would start every external locate half a frame behind on average.
Why "at or after": a clip spans `[start, end)`. The first frame that shows a clip starting at `s` is `ceil(s / step)`.
Rejected: nearest rounding for user seeks, because a seek to a clip start could land just before it and show black.

**D5 — One tick dt for everything that renders.** `tick_dt = frames × step`, in every transport state. It feeds generators, effects, the compositor, modulation's audio followers, audio triggers and video `pre_render`. Export frame 0 has `frames = 0`, so `tick_dt = 0` as today.
Rejected: a zero dt while paused. Paused graphs would freeze.

**D6 — External sources steer the beat with a slow, capped tempo trim. A big error seeks. Time never leaves the grid.** Peter, 2026-10-14, with the review rulings of the same day. This replaces the 2026-10-13 held/skipped-frame rule and P1's phase lock.
- Under an external authority (Link, CLK, AbletonOSC, timecode), each tick advances the index by exactly `frames`. The index never holds, skips or jumps for phase. Time stays `index × step` (D1).
- The beat comes from the **follow segment**: `beat(t) = anchor_beat + (t − anchor_time) × rate`, with `rate = base_bpm / 60 × (1 + trim)`. `base_bpm` is the source's tempo (D21). `trim` is the follower's correction. D8 makes the segment the engine's converter under an external authority.
- **Two outcomes only: trim or seek.** There is no snap. An error the trim can remove is trimmed. An error past 50 ms that lasts goes through the seek path, Link included. Nothing moves the beat backward except a seek.
- **The loop.** One sample per tick, taken after the advance. `t` is the grid time of the frame this tick shows. `Δt` is the grid time since the last sample (`frames × step`). `b_src` is the source's beat at the frame's read instant (D7, D19). `b = beat(t)` before the update.
  - Phase error in seconds: `e = (b_src − b) × 60 / base_bpm`.
  - Pre-filter: `e_f += (1 − exp(−Δt / τ)) × (e − e_f)`.
  - Integral, by conditional integration: when the trim sits at its cap and `e_f` pushes it further the same way, `I` is frozen; otherwise `I = clamp(I + K_i × e_f × Δt, −I_max, I_max)`.
  - Trim: `trim = clamp(K_p × e_f + I, −C, C)`.
  - Re-anchor the segment at `t` with the new rate. The beat at `t` does not change, so frame N keeps the beat it had before the sample; the new rate runs from N to N + 1.
  - `K_p = 2ζω_n`, `K_i = ω_n²`. This is the second-order loop of the delay-locked loop JACK uses (F. Adriaensen, "Using a DLL to filter time", 2005), with one change. The classic DLL also moves the phase by `b × e` on every sample, which would make the beat jump a little each frame. Here every correction goes through the rate, so the beat stays continuous. With ζ = 1 the loop does not ring, but a PI loop overshoots once: by about 17% of a step small enough to stay off the cap (0.35 ms after a 2 ms step), and by the same 0.35 ms after larger steps, where the cap holds the trim and the frozen integral cannot wind up. `Δt = 0` (a tick with nothing advanced) updates nothing.
- **Constants.** Two rows, one per source class:

| | Fast row: Link, CLK | Slow row: AbletonOSC, timecode |
|---|---|---|
| ω_n, ζ | 1.0 rad/s, 1 | 0.2 rad/s, 1 |
| K_p, K_i | 2.0 /s, 1.0 /s² | 0.4 /s, 0.04 /s² |
| Pre-filter τ | 0.1 s | 0.5 s |
| Trim cap C | ±0.5% | ±0.5% |
| Integral clamp I_max | ±0.1% | ±0.05% |
| Seek | error past 50 ms for 100 ms | error past 50 ms for 250 ms |
| Follows errors that last | about 1 s or more | about 5 s or more |

- **The catch-up window**, the one exception to the cap. It opens when the base tempo moves more than 2% within 0.25 s: a real tempo change, never noise. It stays open 1 s from the last such move, then as long as the error stays above 3 ms, never more than 3 s in all. Inside it:
  - the cap is `max(0.5%, half the relative tempo change)`, never above 25%: 13% for 174 → 128, 25% for 140 → 70;
  - `K_p` doubles;
  - a seek needs an error past 150 ms, not 50 ms.
  - **Plainly: the trim exceeds 0.5% only inside a confirmed tempo change.** There the music itself has just changed speed by more than 2%, and the beat runs at most half that change away from the new tempo while it catches up. It stays monotonic and smooth. Steady play never opens the window: zero opens after warm-up in 4 hours of CLK, an hour each of Link and AbletonOSC (section 5 (Proof plan), P3 proofs).
  - Why: a clock that learns a tempo change late runs on at the old tempo, and with a 0.5% cap it then needs seconds to close the gap, or it seeks backward. Under CLK alone, 174 → 128 used to snap backward in every run and double-start the clip on the downbeat. With the window it is trimmed in all runs, with no backward frame (D20). Peter ruled Q5 "fix" on 2026-10-14.
- **Why these numbers.** They come from scratchpad simulations of this loop against the D7 noise models: 60 fps, seeded jitter, one-hour runs for the steady cases. P3's tests re-measure them in Rust (section 5 (Proof plan), P3 proofs).
  - ω_n = 1 rad/s for Link and CLK. It removes a 10 ms error in under 2 s and keeps ±1 ms of tick jitter out of the beat speed (under 0.15%).
  - ω_n = 0.2 rad/s for AbletonOSC and timecode. Their noise is about ten times larger: 10 Hz reports with Live-side skew, and 25 fps quantising.
  - The pre-filter stays. Without it the slow row passes each AbletonOSC report's skew straight into the beat speed: the trim sat at its 0.5% cap and the speed changed by 0.83% between frames, against 0.25% and 0.02% with it. LiveMTC timecode went from 0.05% to 1.0% per frame, a trim flipping between its caps. In the fast row it halves the frame-to-frame speed change under CLK. It costs about 2° of phase margin.
  - Cap ±0.5%. It must hold the steady rate gap between Ableton's audio clock and the Mac's clock (about ±100 ppm, 0.01%) plus the tempo estimate's error (under 0.05%), with room to remove phase error at 5 ms per second. 0.5% is 0.6 BPM at 120. The eye notices a change in motion speed somewhere around 5–10%; 0.5% is ten times below that.
  - Integral clamp 0.1% (0.05% slow). The integral only has to hold the steady rate gap. Clamped at the cap, as in the first revision, it wound up: a 5 ms step took 2.3 s to settle with 1 ms of overshoot. Conditional integration plus the small clamp gives 0.85 s and 0.4 ms.
  - The proportional term alone reaches the cap at 2.5 ms of error (fast row) and 12.5 ms (slow row). Above that, phase error falls at 5 ms per second.
  - Seek after 50 ms held for 100 ms (fast) or 250 ms (slow). Below 50 ms the trim removes the error in under 10 s. The hold keeps one late AbletonOSC report or a burst of late ticks from seeking.
- **Settle time** (simulation, from a phase step, with an 80 ppm rate gap). Fast row: 5 ms is under 1 ms after 0.85 s, 10 ms after 1.8 s, 20 ms after 3.8 s, 40 ms after 7.7 s. Overshoot 0.35–0.4 ms. Slow row: 10, 20 and 40 ms are under 4 ms after 2.2, 3.7 and 7.5 s, and under 1 ms after 12, 13 and 17 s.
- **Seek.** A seek bumps the epoch, runs the seek side effects, moves the index to the frame nearest the target's map time with no hold (D4), and seeds the follower at the source's beat with the integral kept: the rate gap it learned is still true after a relocation. Two kinds:
  - The source's own seek, at its existing threshold, unchanged: MIDI clock 2.0 s plus the tick-delta range check (`midi_clock_sync.rs:768-772`, `:783`), timecode 0.5 s (`osc_sync.rs:421`), AbletonOSC 2.0 s (`transport_sync.rs:31`). Sources now pass the target as a beat.
  - The follower's seek: the error has stayed past 50 ms (150 ms inside a catch-up window) for the row's hold time. `observe` returns `FollowEvent::Seek { beat }` and the engine runs the same seek path. This gives Link its first seek path, so a large Link disagreement (after an export, a stalled session) is handled like any relocation.
- **Seed.** The first sample after a seed point sets the segment to `(t, b_src, base_bpm)` with `trim = 0`, `I = 0` and the pre-filter cleared. Seed points: Play or resume under an external authority, every external seek, the end of an export (D9), and a tempo-map edit under timecode (D11). Until the first fresh sample after a seed point, the segment free-wheels from where it was. The seed then moves the beat to the source's position once: under CLK on the first tick, at most one tick period after Play; under Link on the same tick. That move belongs to starting or seeking, like today's Play alignment to MIDI clock (`content_commands.rs:894-911`).
- **Link takes its beat when its tempo changes.** Link's beat is exact at any instant. On the frame where Link's session tempo differs from the last sample's, the segment re-anchors at `(t, b_src)` with the new tempo, keeping trim and integral. The frame's increment then lies between the old and the new tempo's increments, so the beat stays monotonic and no phase error is left (simulated 0.00 ms on every jump and ramp; the first revision left 2–16 ms). The sample carries `exact: true` only from Link.
- **Handover from Internal while playing** is not a seed. The segment starts at the current frame's own beat and the source's tempo, so nothing moves on that frame. The source's first samples then trim or seek like any others.
- **Free-wheel.** A tick with no sample (gated, stale or not yet arrived) leaves the segment alone, so the beat runs on at its last rate. Today's gates stay: the seek cooldown and the pending AbletonOSC ack for CLK (`content_thread.rs:1997-2003`), and MANIFOLD owning playback for Link (`:1977`). A CLK source goes stale after four tick periods with no tick (D20), so a clock gap free-wheels instead of extrapolating.
- **Handover between external sources** is bumpless, the standard control term for switching controllers without a kick: the segment is kept, `I = clamp(trim, −I_max, I_max)`, the pre-filter clears, the new source's row applies. A disagreement the new source then holds past 50 ms seeks.
- **What the follower guarantees while playing.** The beat is continuous; only seeks jump it. The beat strictly increases between seeks, because `rate ≥ 20 / 60 × (1 − 0.25) > 0` even at the largest catch-up cap. Outside a catch-up window the beat speed is within 0.5% of `base_bpm`. Per-source phase and speed bounds are in D7.
- **What is left of whole-frame correction.** Nothing. `Correction` (Hold, Skip, Jump), `PhaseLock`, `Smoothing`, `observe_external`, `clear_external`, `last_correction`, and the stopgap's `follow_external`, `defer_external` and `take_pending_external` are deleted, not kept beside the follower. `PhaseLock`'s slew limit becomes the trim cap; its outlier rejection becomes the seek hold; its "first sample seeds" carries over as the seed. Under an external authority the only index placements are seeks (D4).
- Rejected:
  - Held or skipped frames for phase (the 2026-10-13 rule). Each is a visible stutter. Peter replaced it.
  - Snaps, the first revision's middle outcome. A backward snap tripped the backward-move guard (`engine.rs:1815-1824`) and restarted a clip that had just started on a downbeat. A snap also skipped the seek side effects, and Link had no other path. Trim or seek covers every case.
  - Moving the phase directly on each sample, as the classic DLL does. The beat would jump a little every frame.
  - No cap. Source noise would reach the beat speed, and a large error would visibly speed up the show.
  - Trimming time instead of the beat. Time is the grid (D1).
  - Copying Link's beat onto every frame. It would carry each step of Link's network clock into the beat. Link's beat is taken only on its tempo changes, where the trim would otherwise lag.

**D7 — Each source is read at the frame's screen instant and projected from timestamped arrivals.** The read instant is `R = deadline + L`: the deadline of the frame this tick shows (`FrameTimer::tick_deadline`, `frame_timer.rs:438-442`) plus the output latency (D19). A non-Link source also adds its learned offset against Link (D19). Each source hands the engine one sample per tick: `ExternalSample { beat, bpm, fresh, exact }`, the source's beat at `R`, its base tempo (D21), whether it carries new data, and whether its beat is exact (Link only).
- **Link.** `beat_at_time(R)` minus the cached offset (D15), with `R` mapped into Link's clock as today (`link_micros_at`, `link_sync.rs:184-193`). Exact at any instant; fresh every tick. Noise model: no jitter; the session tempo runs up to ±100 ppm off the Mac's clock; Link's clock sync shifts the timeline by up to ±0.2 ms every 30 s. ⚠ VERIFY-AT-IMPL: the last two are assumptions about Link's network-time filter, not read from its source; the trim absorbs any constant gap inside the cap and any step under 50 ms.
- **MIDI clock.** The D20 estimator gives position and period at `R`: `beat = (k0 + (R − t0) / period) / 24`, where `t0` is the filtered arrival of tick `k0`. Handed every tick while ticks arrive. Noise model: arrival jitter of ±1 ms uniform plus 0.3 ms Gaussian, 0.5% of ticks 4 ms late, an 80 ppm rate gap, and delivery stalls of up to 600 ms followed by a burst.
- **AbletonOSC.** The receive thread stamps an `Instant` on each packet (today the frame's drain time is used, audit item 1.10). `beat = reported beat + (R − arrival) × bpm / 60`, at Live's tempo from the tempo listener (D8). The tempo listener is stamped too: when Live's tempo changed after the report, the beat runs at the old tempo until the tempo report arrived and at the new one after, so a report from before a tempo change is not projected at the new tempo. No one-way delay is added: the relay's round-trip estimate (`transport_sync.rs:32-37`) times a command to its acknowledgement, which includes Live's scheduler and the listener cadence and is clamped at 50 ms, so half of it would put the beat 25 ms or more ahead. The wire delay on one machine is under a millisecond; any constant left is D19's offset to learn. The latest-only slot (`ableton_bridge.rs:907-910`) keeps the latest report with its stamp. Its projection is handed every tick while reports arrive and the relay hands position over (D6 takes one sample per tick; observing only on a new report steps the pre-filter by 18% per report and the beat speed by about 0.07% at once). Noise model: reports at 10 Hz ± 20 ms, each up to ±10 ms off Live's audio position, ±1 ms network jitter, and the tempo listener arriving 0–200 ms after Live's tempo changes. ⚠ VERIFY-AT-IMPL on the rig: the skew and the listener lag are guesses; C0's rig log measures both.
- **Timecode.** The receive thread stamps each packet. Song time at `R` is `s = seconds + (R − arrival)`. Then `beat = map(s)` and `bpm = 60 × (map(s + step) − map(s)) / step`: the map's tempo over the next frame, so a tempo point in the map is followed exactly. One-float song seconds keep `f64` when sent as an OSC double. H:M:S:F gains half a timecode frame: a frame label means the frame has started, so on average the true time is half a frame later. The latest message's projection is handed every tick while timecode arrives. Noise model: LiveMTC labels 25 fps frames, sent on a free-running metro; arrival jitter up to 1 ms.
  - The sender is LiveMTC, not ours (audit item 1.10). P3 changes only the receiver. A MANIFOLD-owned sender of unquantised song seconds is deferred; it would bring timecode down to the "unquantised" row below.
- **Bounds.** Simulated worst case at steady tempo after a 20 s warm-up, and the bound P3's tests assert (about twice the simulated worst). "Speed" is the shown beat's increment over a frame against the music's increment over the same frame; "per frame" is how much that ratio changes from one frame to the next.

| Source | Simulated worst (phase; speed; per frame) | Test bound: phase | Test bound: speed | Test bound: per frame |
|---|---|---|---|---|
| Link | 0.36 ms; 0.06%; 0.011% (1 h; phase against Link's own timeline, which Live also plays on) | 1 ms | 0.15% | 0.03% |
| CLK, Link tempo latched | 0.37 ms; 0.08%; 0.013% (1 h) | 1 ms | 0.2% | 0.03% |
| CLK alone | 0.61 ms; 0.14%; 0.037% (1 h) | 1.5 ms | 0.3% | 0.08% |
| AbletonOSC | 3.5 ms; 0.28%; 0.021% (1 h) | 8 ms | 0.6% | 0.05% |
| Timecode, unquantised seconds | 0.59 ms; 0.01%; 0.001% (5 min, six metros) | 2 ms | 0.1% | 0.01% |
| Timecode, LiveMTC 25 fps labels | 15.7 ms; 0.5%; 0.053% (10 min, six metros) | 21 ms | 0.6% | 0.1% |

- What the bounds mean on stage. At 60 fps a frame is 16.7 ms. Under Link and CLK the beat is within a millisecond of Ableton's, under a tenth of a frame. A cut lands on another frame than a perfect clock would pick only when Ableton's beat falls within that millisecond of a frame boundary, and then by one frame. AbletonOSC is the degraded fallback tier and stays within a quarter frame. LiveMTC timecode is limited by its own frame labels.
- Tempo changes have their own bounds: D20 for CLK, D22 for the others. Every one of them is monotonic with no backward frame.
- Sources do no other filtering. The follower owns phase filtering; the D20 estimator is the one exception, because tick timing must be turned into a position and a tempo first.

**D8 — While Ableton drives, the beat follows Ableton's tempo through the follow segment. Under Internal, the map is the truth. Time is never converted from a distant beat through the segment.** Peter's ruling, 2026-10-13, extended to all four external sources and to hand-back on 2026-10-14.
- Time is always the grid.
- The engine has one converter, `TempoSource`:
  - `Map { beat_offset }` under Internal: `beat(N) = map(time(N)) + beat_offset`. The offset is 0 except after a hand-back (below).
  - `Follow(ExternalFollower)` under Link, CLK, AbletonOSC or timecode: the follow segment of D6, computed from the index each frame, never accumulated. Its base tempo is Ableton's: Link's session tempo, the CLK estimate, the AbletonOSC tempo listener (`transport_sync.rs:326-331`), or for timecode the map's tempo at the source's song time (D7, D21).
- AbletonOSC was on the map until now because its relay was thought to have no tempo channel. It has one. Following it keeps the beat continuous when CLK dies mid-set and AbletonOSC takes over.
- The anchor is a time, not a frame index. An fps change (D10) or the export grid (D9) remaps indices, and an index anchor would then sit at a different time and jump the beat: anchored at frame 600 at 120 BPM with the playhead at 20 s, `set_fps(30)` read beat 20 instead of 40. The anchor time is always a grid time, so the beat is still a function of the index between samples.
- The segment re-anchors at the shown frame on every sample (D6). An Ableton tempo ramp is a run of one-frame segments.
- **The segment is exact only near now.** Converting a beat from long ago through it extrapolates at today's rate, so every rate change would rescale the whole elapsed time: a clip that started 30 s before a Link change from 120 to 140 BPM would be placed 4.4 s wrong. So the engine never does it. The beat→time conversions on f32e3c5fd, sorted:
  - Ahead of or at the shown frame, safe through the segment: session quantise targets; clip end times for the recently-started gate (`engine.rs:3188-3198`, which inlines the map today and moves to the converter); recording stamps (`live_clip_manager.rs:959`, `:1011`); seeks and the insert cursor (`content_commands.rs:909`, `:963`, `transport_controller.rs:183`, `:195`), which place the index by the map (D4).
  - Durations and loop lengths (`engine.rs:2655-2676`): computed from beat differences by `converter.duration(b0, b1)`, never from two absolute times. Under the map that is `map⁻¹(b1) − map⁻¹(b0)`; under the segment it is `(b1 − b0) / rate`, which carries no anchor error.
  - A clip's start time for video (`engine.rs:2648-2650` via `:2728-2730`): **latched**. When a clip activates, the engine stores `t_act − (b_act − start_beat) / rate`, the converter's answer on the activation frame, where the distance is under a frame. Non-warped video elapsed time is then `time − latched start` and never rescales. Warped clips use beats (`engine.rs:2724-2726`) and already follow. Latches live in an `AHashMap` reserved at load to the clip count; inserts happen only at activation. Every placement (seek, locate, tempo-edit remap, export) re-latches every active clip on that frame. Authority changes and hand-back keep time continuous, so latches survive them.
  - Export (`content_export.rs:808`) and the export restore (`engine.rs:936`): the map, as export always is (D9).
  - So no engine path converts a past beat to an absolute time, and nothing needs a log of past (time, beat) points. The follow converter's `time_at` debug-asserts that its beat is no more than one beat behind the shown frame, so a new caller that would need one fails in tests.
- **Hand-back to Internal keeps time and the beat.** When authority returns to Internal (Link peers vanish, CLK stops with nothing behind it), the index keeps advancing as before. The converter becomes `Map { beat_offset: beat − map(time) }` on that frame, so neither time nor the beat moves. From then on the beat runs at the map's tempo. Feedback, trails, free-time LFOs, generators and video carry on through a network hiccup.
  - The offset clears at the next Stop, seek, Play or tempo-map edit. There the beat is kept and the index moves to the first frame at or after `map⁻¹(beat)`, at a moment that already restarts things.
  - Handover from Internal to an external source with an offset in place starts the segment at the frame's beat, offset included, and clears the offset. Nothing moves.
- Export always uses the map (D9). An export started while Ableton drives renders the arrangement, not the live follow.

**D9 — Export shares the clock, set before anything moves.**
- `begin_export_clock(fps)` runs before the first seek (`content_export.rs:717`). The two seeks, the warm-up ticks and the frame loop all run on the export grid at export fps.
- Export frame k sets the index to `index_at_or_after(start) + k` on the song-time-0 grid at export fps (D17), so live and export sample identical times.
- In export mode the tick sets the index regardless of external sync, so an export under CLK or timecode advances (audit item 1.1).
- Bit-identity with old export is not a goal. Live equals export is the requirement. D14 changes on-frame edge results in export as well as live.
- The end of export restores the live rate and seeks to the restore time as today (`content_export.rs:427`, `:497`). Under an external authority the end of export is a seed point (D6): the follower resumes at the source's beat, not from an anchor set before the export.

**D10 — One source of truth for fps.** `project.settings.frame_rate` is the only fps. Each tick, outside export, the engine compares its clock fps with it and remaps when they differ. It keeps the current time and moves the index to the first frame at or after it, with no hold. The content thread then retargets the timer from `engine.frame_rate()` when the two differ. The four `timer.set_target_fps` copies in `content_commands.rs` are deleted. The rt-capture override becomes a project setting written before load.
The clock and the tick grid accept 1 to 1000 fps through one rule (`sanitize_fps` in `transport_clock.rs`). Anything else, including NaN and infinity, runs at 60 with a one-time warning naming the bad value, so a corrupt setting never stops the transport.

**D11 — Tempo edits during play keep the musical position.** Under Internal authority, after a map or BPM edit, the beat is kept and the index remaps to the first frame at or after `map'(beat)`, with no hold. Under Link, CLK or AbletonOSC the segment governs the beat, so a map edit changes nothing on screen until hand-back (D8). Under OSC timecode, song time is the master and the source's beat is `map(seconds)`, so a map edit is a seed point (D6): the next sample re-seeds the segment, and the beat moves at once to the new map's beat for the same song time. Under Internal with a hand-back beat offset in place (D8), the edit also clears the offset. The `external_time_sync` early return in `reconcile_tempo_edit` (`engine.rs:807-810`) goes with the flag.

**D12 — Video picks its frame from transport time.** Each tick, a playing clip's target source frame is `floor(video_time × video_fps)` from `compute_video_time` (`engine.rs:2559`). It requests that frame when it differs from the one shown and seeks when the gap exceeds the existing 3-frame rule. Both integrators go: `time_accumulator` and `playback_time += dt × rate` (`video_renderer.rs:812-813`). Decode stays asynchronous: a late decode holds the previous frame. P4 owns this. P3 already gives non-warped clips a latched start time (D8), so `video_time` never rescales when Ableton's tempo changes.

**D13 — What stays on wall time.** The content timer's own telemetry, `realtime_now` and every deadline built on it (audit item 1.3), the sim late-frame rule, MIDI input guards, sync-controller windows, the UI thread, audio voices and audio analysis. None of these may read the transport frame index.

**D14 — A frame's beat is sampled a ten-millionth of a frame late.** The engine computes the one beat per frame as `beat_at(time(index) + 1e-7 × step)`. Time itself stays exact. An edge that sits exactly on frame k then always counts as reached on frame k. That covers clip edges, automation steps, session quantise targets and one-shot expiry, with one rule at one site. A nudge of 1e-7 frames is 1.7 ns at 60 fps, 2,000 times the worst rounding error (3.6e-12 beats) and far below anything visible. Scripted over every on-frame 1/16 edge at 60, 50, 30, 25 and 24 fps and 120–180 BPM: the matching frame rule `ceil(edge_time / step − 1e-7)` selects the right frame for all of them. It moves no off-frame edge.
Rejected: comparing each edge as an integer frame against the index. It is the same rule, but it would put an fps into `Layer`'s beat queries and into every other comparer.
The sampled beat is for comparisons only. **Actions are recorded at the frame's exact beat**, `beat(time(index))` (`PlaybackEngine::frame_beat`): a session launch or stop that takes effect now, a return to arrangement, an automation touch, a saved position, a recorded tempo point. A later frame's sampled beat then reaches them on the frame they belong to, at any fps. Recorded at the sampled beat instead, an unquantised 2-beat loop at 120 BPM restarted at 59 to 61 frame intervals, and about one export restore in ten came back a frame late.
Consequences:
- A quantised press rounds up from the sampled beat, not the exact one. The sampled beat is reliably past a boundary on this frame, so a press on a downbeat always waits for the next one; the exact beat would round either way.
- An automation take ends at the last touched frame's sampled beat: the take holds its last value through that instant, then the old curve resumes. Replay reads the last touch on its own frame.
- Clip-control binding cutoffs compare against the frame's floor beat, `beat(time − 1e-7 × step)`. An authored edge on this frame computes a hair either side of the frame's beat, so the floor keeps it as new.
- MIDI phantom clips keep the sampled beat. They become visible on the next frame either way, and one-shot stamps are `f32`, far coarser than the offset.
- The offset is a fraction of the clock's own frame. An export at a rate other than the project's matches live time bit for bit, and its beat to within the offset.

**D15 — The Link offset is taken at the read instant, on every start, and a Link-initiated start aligns to the quantum.** MANIFOLD's beat under Link is `link_beat(R) − offset` (D7).
- The offset is computed from the same Link read as the sample: `link_beat(R) − b`, where `b` is the beat of the frame this tick shows. Today it pairs the previous poll with the engine's current time (`content_thread.rs:2020-2034`), so it carries up to one poll of error.
- It is taken at every transition into Playing while Link is the authority, whoever caused it:
  - MANIFOLD's own Play, Seek or SeekToBeat (`content_commands.rs:913`, `:959`, `:971`): the exact offset. MANIFOLD chose its position, so it keeps it.
  - A Link-initiated play (`link_sync.rs:153-155`): the offset rounds to a whole multiple of Link's quantum (4 beats, `link_sync.rs:44`), per Peter's ruling of 2026-10-13. The follower then seeds at `link_beat(R) − offset`, so MANIFOLD's bar lines up with Link's bar. The start moves by at most half a quantum. This is a seed, not a snap: the transport is only starting.
  - Link becoming the authority while MANIFOLD already plays (peers appear): the exact offset, so the beat does not move. MANIFOLD's bar stays where it was; the next Link-initiated start aligns it.
- Stop and project load still clear the offset (`content_commands.rs:937`, `:1425`). The next start of either kind takes it again, so Link always steers after a Stop. That fixes BUG-0u1al (Link-initiated Play never caches the Link beat offset).
- While the offset is not set, Link supplies no sample and the follower free-wheels (D6).
- As built (C8): a start or seek requests the offset and the next tick takes it, after Link's read, against the beat of the frame that tick shows (`external_sync.rs`, `LinkOffset`). So a command between ticks and a Link start are measured the same way.
- A Link disagreement past 50 ms that lasts 100 ms seeks like any relocation (D6). Before P3, Link had no seek path.

**D16 — What the determinism claim covers.** Bit-equal per frame index on two runs, whatever the press phase, jitter or stalls: time, beat, clip membership, automation, LFOs and drivers, session launches, one-shot expiry, and every DERIVED reader in audit item 1.2. Integrators fed tick dt are bit-equal only when the two runs dropped the same frames (audit item 1.3). Video frames: not until P4. Physics and liquid: never. The sim's live cap and reanchor read wall load (`clock.rs:125-130`, `:380`), so water under load differs between runs. That is LIVE_SIM_CLOCK_DESIGN's choice of slow motion over stutter, and this design does not reopen it.
Under an external authority the claim is different, because the beat follows Ableton's timing. Time per index is still exact. The beat is continuous, strictly increasing between seeks, and within D7's bounds of the source. Two plays of the same section match each other only as closely as Ableton's own timing and the arrival jitter allow.

**D17 — Export runs on the song-time-0 grid.** Peter's ruling on Q2, relayed 2026-10-14. Export frame k is the first export-grid frame at or after the export start, plus k. Live and export then sample identical times for the same song moment. An export started off the grid begins up to one frame after its marker: at 128 BPM and 60 fps, half a frame on every odd bar.
Rejected: frame 0 exactly on the marker. Export would then sample times live play never renders. Putting both on the marker would mean anchoring the live grid wherever Play was pressed, which is the non-determinism this bead removes.
Consequence: the clock has no origin (D1). `begin_export(fps)` and `end_export(fps)` change the step and keep the current time.

**D18 — Bad input never moves or breaks the clock.** `locate`, `seek_playing` and `locate_nearest` ignore non-finite times. The follower ignores a sample whose beat or tempo is not finite, and clamps tempo to 20–300 BPM as the map does. The extra output delay accepts 0–250 ms, learned source offsets ±50 ms, and a measured present delay is adopted only from a median (D19). Absurd finite times clamp the index to ±2^52, where `index × step` is still exact. An absurd finite sample moves the beat by at most the trim cap until it has lasted the seek hold, and the integral is frozen while the trim sits at its cap, so one wild sample costs at most a few milliseconds of phase, which the trim then removes; a lasting one seeks. Index sums saturate, and the tick grid's deadline indices stop at 2^32, so no arithmetic overflows and no `Duration` conversion sees a bad value.

**D19 — Output latency is measured, not set. Every external source is read for the moment the frame reaches the screen.** Peter, 2026-10-14: MANIFOLD measures its own delays and sync offsets; nobody should have to set anything.
- The read instant is `R = deadline + L` (D7), with `L = L_present + L_display + L_extra`. This is Link's guidance for a peer with output latency: ask for the beat at the moment the output is perceived, not when it is computed. Internal authority never reads `L`.
- **`L_present`, measured every frame, applied slowly.** The stage output window is presented from the content thread, in the same command buffer that renders the frame (`content_pipeline.rs:3332-3362`). So the content thread knows which frame and which deadline each present carries. A presented handler on that drawable (`MTLDrawable` `addPresentedHandler` and `presentedTime`, macOS 10.15.4 and later) reports the host time the frame went on screen. `manifold-gpu` wraps it as `GpuDrawable::on_presented`, the same shape as the per-frame completed handler (`encoder.rs:2695`). The handler sends `ContentCommand::FramePresented { frame, at }` on the existing command channel.
  - `L_present` is the median of `presentedTime − deadline` over the last presented frames (from 120, growing to 600: over 120 alone the median wanders more than half the adopt step under ±3 ms of jitter), recomputed once a second, and adopted only when it moves by more than 1 ms. A dropped frame (`presentedTime` 0) is skipped. Presentation jitter therefore never reaches the beat; a real change (another display, a new refresh rate) enters as a phase step the trim removes.
  - `presentedTime` is host time in seconds on the `mach_absolute_time` base. It maps to `Instant` with one paired read of both clocks. ⚠ VERIFY-AT-IMPL, as for the midir stamps (D20).
  - With no output window open, or before the first 120 presents, `L_present` is 17 ms, one frame at 60 fps.
  - It covers MANIFOLD's render, the wait for the display's refresh, and the compositor. It ends when the refresh that carries the frame starts, at the top of the picture.
- **`L_display`, read from the display when it says.** macOS has no public API for an external display's own processing delay. Checked on this Mac (M4 Max, built-in XDR panel, 2026-10-14): `CVDisplayLinkGetOutputVideoLatency` returns 0/0 with no valid flags, which means unknown; the IORegistry shows `DisplayAttributes` and `ProductAttributes` but no raw `EDID` key. HDMI displays may state a video latency in the HDMI vendor block of their EDID. The field is optional, and many projectors and LED processors leave it empty.
  - The plan: on a display change, read `CVDisplayLinkGetOutputVideoLatency` for the output window's display and, if the IORegistry exposes that display's raw EDID, its HDMI video-latency byte. Use the first valid value; otherwise `L_display = 0`. No private API (the `IOAVService` EDID calls some tools use).
  - ⚠ VERIFY on the rig with the projector or LED processor attached. C0 logs both reads. If neither reports a value, nothing automatic covers the display's delay.
- **Per-source offsets, learned from the sources themselves.** One `L` cannot align every source. Live publishes Link's timeline for its own output: Link is speaker time. MIDI clock carries Live's per-port sync delay. AbletonOSC and timecode report engine position without output compensation (⚠ VERIFY on the rig).
  - So Link is the reference. While Link and another source both deliver fresh samples, the content thread compares their beats at the same `R`, every tick. The source's offset is the mean difference, in seconds, over 30 s of steady play: no catch-up window open, both sources fresh, differences under 50 ms. It is adopted when the spread over that window is under 2 ms and it differs from the current offset by more than 1 ms. That source is then read at `R + offset`.
  - Offsets are bounded to ±50 ms. Each change enters as a phase step through the trim.
  - The content thread reports learned offsets in `ContentState`. The UI thread saves them per rig in `UserPrefs` (`MANIFOLD_SyncOffsetClkMs`, `MANIFOLD_SyncOffsetOscMs`, `MANIFOLD_SyncOffsetTimecodeMs`). So a later set without Link keeps the last learned value. At startup the UI sends the stored values before the content thread's first tick.
  - This is the computation C0's rig log already prints (section 6 (Phasing), P3). C9 turns the printout into the adopted offset.
- As built (C9): `sync_calibration.rs` holds `L` and the learners; the presented handler moved from the rig log to `present_feedback.rs`, which also sends `FramePresented` to the content thread. The source is read at `R + offset` and the same read both feeds the follower and is compared with Link, so the learner works on the residual. `L_display` is CoreVideo's value only; the EDID video-latency byte is not read yet (BUG-3o90s (EDID video latency for L_display)). `sync_prefs.rs` checks the output window's display once a second and saves learned offsets.
- **What cannot be measured automatically.**
  - A display's processing delay when it reports none.
  - The sound path after Live: the PA's processing and the distance from speaker to audience.
  - Whether Live's own output-latency compensation is right. Link inherits Live's view, and nothing in MANIFOLD can see past it.
  - Any source offset while Link is absent, beyond the last learned value.
- **`L_extra`, the override.** `UserPrefs` key `MANIFOLD_ExtraOutputDelayMs`, default 0, range 0–250 ms. Anything else, non-finite included, loads as 0 with a one-time warning naming the value. It is not in the normal workflow: no footer field and no menu entry. The five-second rig log line prints `L` and its parts, so the number is there when someone needs it. Calibrating it: a MANIFOLD clip that flashes on every beat, a click in Live on every beat, a phone filming screen and speaker at 240 fps; raise it by however late the flash is.
- **Against today.** The P2 stopgap and every build before it show the source one frame ahead by accident (audit item 1.10). That hides about one frame of pipeline delay. After P3 the measured `L_present`, typically one to two frames, replaces the accident with a measurement. Q4 is closed.
- Rejected:
  - A visible latency field (Peter: nothing to set).
  - Applying a per-frame measurement directly: presentation jitter would reach the beat.
  - Hand-set per-source offsets: the sources measure them.
  - Private display APIs.
  - Compensating Link only.

**D20 — MIDI clock: timestamped events, MIDI 1.0 position rules, and a fixed-bandwidth tick filter with a confirmed-change refit.**
- **Events.** The midir callback sends every clock message with its time through a `std::sync::mpsc` channel, the note input's shape (`midi_input.rs:115`, `:470`): `ClockEvent { kind: Tick | Start | Continue | Stop | SongPosition(u16), at: Instant }`. `at` comes from the CoreMIDI packet timestamp that midir passes (`midi_clock_sync.rs:134`, unused today), mapped to `Instant` by taking the smallest delivery lag seen as zero (`StampMap`), so the mapping needs no knowledge of the stamp's base. Only if that timestamp is missing is it `Instant::now()` in the callback, which turns a delivery stall into a burst. ⚠ VERIFY-AT-IMPL: midir's timestamp base on macOS. The packed `AtomicU64` state and its CAS loop (`midi_clock_sync.rs:19-78`) are deleted. The content thread rebuilds the position from the events, in order, each tick.
- **Position rules (MIDI 1.0).** Start arms position 0. Continue arms the current position. Song Position Pointer sets the position in sixteenths. The first Tick after Start or Continue lands on the armed position; each later Tick adds 1/24 beat. Ticks while stopped never move the position. Today every Tick counts, so the beat runs one tick early (audit item 1.10). ⚠ VERIFY-AT-IMPL with C0's raw byte log from Live: whether Live sends ticks while stopped, and whether it sends Song Position Pointer on a loop wrap.
- **Estimator.** A second-order DLL on tick times, in Adriaensen's form, with one fixed bandwidth `B = 0.25 Hz`: the predicted time of tick `k` is `t0 + (k − k0) × period`; `e = arrival − predicted`; `t0 = predicted + b·e`; `period += c·e / (k − k0)`; `k0 = k`; with `ω = 2π·B·period`, `b = √2·ω`, `c = ω²`.
  - **Guards.** A tick that arrives less than a quarter period after the previous one is not fed to the filter; it still counts for position. A tick more than 1.5 periods after the previous one waits for the next: a burst's head is as late as the stall, so it is dropped with the burst, and a real tempo drop is fed one tick later. The period is clamped to 20–300 BPM, which also bounds the loop gain, since `ω` grows with the period. Today's tick-delta range check (`midi_clock_sync.rs:768-769`) stays as a seek, and a backward delta never reaches the filter (today's rule, pinned by the test at `:957`).
  - **Refit on a confirmed change.** When three accepted ticks in a row are more than 2 ms off on the same side, each further off than the last, the tempo has changed. The period is refit by least squares over those three ticks and the phase restarts at the last. Then the 0.25 Hz filter carries on. A single late tick (4 ms) cannot make three growing misses.
  - **With Link latched (D21),** the period is Link's at each tick and only phase is filtered. A tick clock alone cannot know a tempo change before its ticks show it; Link knows at the instant.
  - **Seed.** On Start, Continue or Song Position Pointer, the phase starts at the first tick. The period starts from the best tempo already known: Link's if latched, else the last CLK estimate, else the map's. A wrong seed costs one refit.
  - Position at `R` is `(k0 + (R − t0) / period) / 24` (D7). Tempo is `60 / (24 × period)`.
  - Stale after four periods with no tick: no sample; the follower free-wheels.
- **Why this estimator** (simulation, 80 ppm, the D7 jitter model, the D6 follower with its catch-up window, peaks over nine runs):

| Estimator | Steady speed (1 h, 128) | 120 → 140 | 128 → 174 | 174 → 128 | 140 → 70 | Stall 600 ms then burst, stamped on arrival |
|---|---|---|---|---|---|---|
| Fixed 1 Hz | 0.76% | 24 ms | 43 ms | 72 ms | 225 ms | 194 ms |
| Fixed 0.5 Hz | 0.24% | 50 ms | 82 ms | 134 ms | 354 ms | 78 ms |
| First revision's adaptive (0.25 / 4 Hz) | 0.10% | 12 ms | 16 ms | 30 ms | 125 ms | diverges: the period goes negative at 150 ms and infinite at 200 ms |
| **Fixed 0.25 Hz plus refit (chosen)** | **0.14%** | **12 ms** | **17 ms** | **27 ms** | **117 ms** | **13 ms, no seek** |

  A fixed filter alone must trade steady noise against lag on a tempo change: the lag is about `1 / (2πB)` seconds, and the follower cannot be better than its estimate. The refit removes the lag without a second bandwidth, and the guards keep a stall from feeding the filter. Today's estimator, a 96-tick window with an EMA polled once per frame in `f32` (audit item 1.10), lags about 2 s.
- **Tempo changes under CLK** (chosen estimator, peak phase error over nine runs; time until back under 2 ms):

| Event | CLK, Link tempo latched (D21) | CLK alone |
|---|---|---|
| Jump 120 → 140 BPM | 1.5 ms | 12 ms; 0.4 s |
| Jump 128 → 174 | 2.6 ms | 17 ms; 2.6 s |
| Jump 174 → 128 | 3.4 ms | 27 ms; 1.8 s |
| Jump 140 → 70 | 9.7 ms | 117 ms; 0.8 s, about 0.6 s of it more than a frame off |
| Jump 128 → 120 | 0.6 ms | 7.7 ms |
| Ramp 120 → 128 over 16 beats | 0.2 ms | 6.0 ms |
| Ramp 120 → 174 over 8 s | 0.3 ms | 7.8 ms |
| Ramp 174 → 120 over 4 s | 0.5 ms | 9.9 ms |

  Every case: no seek, no frame where the beat goes backward. A clock alone still runs ahead after a drop until its ticks show the change. The catch-up window then slows the beat by up to half the drop for under a second instead of snapping back (D6). Delivery stalls of 150–600 ms followed by a burst, stamped on arrival: peak 3.5–13 ms, no seek, the period inside 20.0–21.8 ms (true 20.83).

**D21 — The base tempo comes from Link when Link and Ableton agree. Live tempo lasts one tick.**
- Base tempo per authority:
  - Link: the session tempo.
  - CLK and AbletonOSC: Link's session tempo while the guard is latched; otherwise the authority's own estimate (D20; the AbletonOSC tempo listener).
  - Timecode: the map's tempo at the source's song time (D7).
- **The guard is a latch, so it holds through tempo changes.** A guard that compared tempos sample by sample would drop Link exactly when Link matters most: during a tempo change, when the estimate lags.
  - CLK compares like with like: Link's beat advance and the ticks counted over the same last 4 s of tick arrivals. A tempo change shows on both sides at once.
  - AbletonOSC compares Link's session tempo with the tempo listener's value. Both are exact values; the listener's lag (D7) is far shorter than the hold below.
  - Latch when they agree within 0.2%. Unlatch only after a disagreement over 0.2% has lasted 2 s, which rides out a stall that shifts one end of the window.
  - Live on Link plays at Link's tempo exactly, so 0.2% is wide for real agreement and tight enough to reject a Link session Live is not part of. Simulated: sessions at 125, 127, 127.5 and 127.8 BPM against Live at 128 never caused a seek (at a 1% threshold, 127 latched and seeked six times in two minutes). With Live on Link, every tempo jump kept the latch (peaks 0.2–0.9 ms).
  - One log line on each latch and unlatch says why.
  - As built (C8): the guard watches whenever Link and a comparable source are both live, not only under that source's authority, so it is already latched when the source takes over (`external_sync.rs`, `LinkTempoGuard`). A session 0.16% off (127.8 against Live at 128) is inside 0.2% and latches; test 13 shows 1.8 ms of phase and no seek, which is the plan's bound for it.
- Why Link first. Link knows a tempo change at the instant it happens; MIDI clock reveals it only through later ticks. The live tempo already ranks Link first (`content_thread.rs:1819-1827`), and ABLETON_TRANSPORT_SYNC_DESIGN.md names Link the tempo plane of Peter's rig.
- Live external tempo (the BPM readout, `get_bpm_at_beat`, clip rates) is cleared at the start of every `tick_sync_controllers` and set only by a source that delivered a tempo this tick. It reports `base_bpm`, never the trimmed rate: the trim is a correction, not a tempo.
- `apply_resolved_tempo`'s Internal-authority branch, which writes the live tempo into the beat-0 tempo point (`content_thread.rs:2131-2151`), is deleted. Under Internal the map is the truth (D8). With live tempo lasting one tick, that branch could only write a stale value. Tempo recording is unchanged. This fixes BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point). It does not depend on the follower: under the stopgap the index already sits at the map time of Link's beat, so deleting the write moves nothing, and it lands early (C1).

**D22 — What each source does on each transport event.** "Seed", "seek", "free-wheel", "bumpless" and "catch-up" are D6's terms.

| Event | Link | MIDI clock | AbletonOSC | Timecode |
|---|---|---|---|---|
| Start | Link-initiated play: arbiter play (`link_sync.rs:153-155`), offset rounded to the quantum, seed (D15). MANIFOLD's own Play: exact offset, seed | Start or Continue: arbiter play as today (`midi_clock_sync.rs:603-611`); seed on the first tick at the armed position (D20) | Relayed Play (`content_thread.rs:1918-1924`); seed on the first fresh report | Timecode arriving means play (`osc_sync.rs:376-382`); seed on the first message |
| Stop or pause | Arbiter pause as today; follower cleared. A MANIFOLD Stop also clears the offset | Stop: arbiter pause; follower cleared | Relayed Pause; follower cleared | 0.5 s of silence means pause (`osc_sync.rs:383-389`); follower cleared |
| Relocate under 50 ms | Trimmed | Trimmed | Trimmed | Trimmed |
| Relocate past 50 ms | Follower seek after 100 ms (new: Link had no seek path) | Song Position Pointer or a bad tick delta seeks at once; otherwise follower seek after 100 ms | Follower seek after 250 ms; the relay's own seek at 2 s | Follower seek after 250 ms; the receiver's own seek at 0.5 s |
| Loop wrap in Live | No effect: MANIFOLD keeps Link's beat, as today | A seek if Live sends Song Position Pointer (⚠ VERIFY-AT-IMPL); otherwise the count runs on past the loop end, as today | Song time jumps back: a seek | A seek |
| Tempo change | Link's beat taken on that frame; no phase error left (D6) | Link tempo at once while latched (D21); else the refit and the catch-up window (D20) | Tempo listener after its lag, plus the catch-up window. With 100 ms of lag: 174 → 128 peaks at 40 ms and is back within a frame after 1.4 s; 140 → 70 peaks at 114 ms, no seek. With no lag: under 3.1 ms | The map's tempo; Live's tempo is not in timecode |
| Tempo ramp | Link's beat on every changed frame (simulated 0.00 ms) | As above; CLK alone peaks under 10 ms | 100 ms of lag: peaks of 3, 11 and 16 ms on the three D20 ramps | The map's tempo points, each followed exactly (D7) |
| Source goes quiet | Peers vanish: authority falls; see the hand-back row | Free-wheel; stale after four periods; authority falls after the 0.5 s timeout | Free-wheel until the receive timeout drops OSC authority | Free-wheel, then pause after 0.5 s |
| Handover to another external source | Bumpless (D6) | Bumpless | Bumpless | Bumpless |
| Hand-back to Internal | Time and beat kept; the beat offset carries until the next Stop, seek, Play or tempo edit; the project's tempo resumes; nothing writes the map (D8, D21) | Same | Same | Same |
| Export | Follower suspended; map beats on the export grid (D8, D9). Export end is a seed point | Same | Same | Same |

## 3. Design

### 3.1 `TransportClock` — `crates/manifold-playback/src/transport_clock.rs` (new)

Owned by `PlaybackEngine`, content thread only. Pure: no `Instant`, no allocation, no tempo map. It lives in playback because the engine owns it and export drives the engine.

Precedents: `manifold_physics::clock::SimulationClock` (`crates/manifold-physics/src/clock.rs:278-400`) for absolute time from an integer count. `Driver::evaluate_frame_aligned` (`driver.rs:189-200`) for the lattice tolerance.

```rust
/// The frame's beat is taken this many frames after its time (D14).
pub const BEAT_SAMPLE_OFFSET_FRAMES: f64 = 1e-7;
/// Seek snap tolerance (D4): wider than the sample offset, so sampled beats round-trip.
pub const SNAP_TOLERANCE_FRAMES: f64 = 2e-7;

/// 1–1000 fps accepted, else 60 with a one-time warning (D10). Shared with `FrameGrid`.
pub(crate) fn sanitize_fps(fps: f32) -> f32;
/// 1.0 / sanitize_fps(fps) as f64: the one step expression for the clock and the tick grid.
pub(crate) fn step_for_fps(fps: f32) -> f64;

/// Transport time as an integer frame index on a fixed step.
/// time(index) = index as f64 * step, never accumulated.
#[derive(Debug, Clone)]
pub struct TransportClock {
    step: f64,            // step_for_fps(fps)
    fps: f32,
    index: i64,           // clamped to ±2^52 (D18)
    hold_next: bool,      // seek while playing: next advance renders `index` first (D4)
    last_advance: i64,
}
// P3 deletes `Correction`, `Smoothing`, `PhaseLock` and the stopgap's
// `pending_external` (D6). The clock never sees an external source.

impl TransportClock {
    pub fn new(fps: f32) -> Self;
    pub fn time(&self) -> Seconds;
    pub fn time_at(&self, index: i64) -> Seconds;
    /// time(index) + 1e-7 * step: the instant the frame's beat is taken at (D14).
    pub fn beat_sample_time(&self) -> Seconds;
    /// time(index) - 1e-7 * step: binding cutoffs (D14).
    pub fn frame_floor_time(&self) -> Seconds;
    pub fn index(&self) -> i64;
    pub fn step(&self) -> Seconds;
    pub fn fps(&self) -> f32;
    pub fn index_at_or_after(&self, t: Seconds) -> i64; // ceil(t/step - 2e-7)
    pub fn index_nearest(&self, t: Seconds) -> i64;
    /// Play, resume, paused seek, tempo edit: snap forward, no hold. Non-finite t ignored.
    pub fn locate(&mut self, t: Seconds);
    /// Seek while playing: snap forward and hold the next advance (D4).
    pub fn seek_playing(&mut self, t: Seconds);
    /// External seek: nearest frame, no hold (D4).
    pub fn locate_nearest(&mut self, t: Seconds);
    pub fn reset(&mut self); // index 0, no hold
    /// Once per playing tick: the seek hold, then `frames`. Never negative.
    pub fn advance(&mut self, frames: u32) -> i64;
    pub fn last_advance(&self) -> i64;
    pub fn set_fps(&mut self, fps: f32); // keeps time, snaps forward (D10)
    pub fn begin_export(&mut self, fps: f32); // song-time-0 grid at export fps (D9, D17)
    pub fn end_export(&mut self, live_fps: f32);
    pub fn set_export_frame(&mut self, first_index: i64, k: u32); // clears a pending hold
}
```

The clock never sees a beat or an external source.

### 3.1b `ExternalFollower` — `crates/manifold-playback/src/external_follow.rs` (new in P3)

Owned by the engine as its converter under an external authority (D8). Pure: no `Instant`, no tempo map, no allocation after construction. `LiveTempo` moves here from `transport_clock.rs` as the follow segment.

```rust
/// One constants row of D6. `FAST` for Link and CLK, `SLOW` for AbletonOSC and timecode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowProfile {
    pub omega_n: f64,        // rad/s
    pub zeta: f64,
    pub prefilter_tau: f64,  // s
    pub trim_cap: f64,       // fraction: 0.005
    pub integral_cap: f64,   // fraction: 0.001 fast, 0.0005 slow
    pub seek_hold: f64,      // s: 0.1 fast, 0.25 slow
}
impl FollowProfile { pub const FAST: Self; pub const SLOW: Self; }

/// D6's shared rules, one place.
pub const SEEK_ERROR: f64 = 0.050;              // s
pub const CATCH_UP_TRIGGER: f64 = 0.02;         // base tempo moved this much...
pub const CATCH_UP_LOOKBACK: f64 = 0.25;        // ...within this many seconds
pub const CATCH_UP_OPEN: f64 = 1.0;             // s from the last trigger
pub const CATCH_UP_HOLD_ERROR: f64 = 0.003;     // s: kept open while the error is above
pub const CATCH_UP_MAX: f64 = 3.0;              // s from the first trigger
pub const CATCH_UP_CAP_MAX: f64 = 0.25;         // fraction
pub const CATCH_UP_SEEK_ERROR: f64 = 0.150;     // s

/// One per tick, from the authority's source at its read instant (D7, D19).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalSample { pub beat: f64, pub bpm: f64, pub fresh: bool, pub exact: bool }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FollowEvent { None, Seeded, Seek { beat: Beats } }

/// beat(t) = anchor_beat + (t - anchor_time) * beats_per_second.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FollowSegment { anchor_time: f64, anchor_beat: f64, beats_per_second: f64 }

#[derive(Debug, Clone)]
pub struct ExternalFollower {
    segment: FollowSegment,
    profile: FollowProfile,
    base_bpm: f64,
    trim: f64,
    integral: f64,
    filtered_error: f64,      // s
    last_sample_time: f64,
    seeded: bool,
    over_since: Option<f64>,  // first sample time past the seek error
    catch_up: CatchUp,        // window times, largest relative change, base-tempo ring
}
/// The base tempos of the last CATCH_UP_LOOKBACK seconds: a fixed [(f64, f64); 256] ring,
/// enough for 1000 fps.
struct CatchUp { /* ... */ }

impl ExternalFollower {
    /// The only constructor: the segment always exists, so the engine's converter
    /// is never undefined. Internal → external handover starts at the frame's own beat (D6).
    pub fn starting_at(profile: FollowProfile, t: Seconds, beat: Beats, bpm: Bpm) -> Self;
    pub fn beat_at(&self, t: Seconds) -> Beats;
    /// Debug-asserts `beat` is no more than one beat behind the last sample (D8).
    pub fn time_at(&self, beat: Beats) -> Seconds;
    /// (b1 - b0) / rate: durations never go through two absolute times (D8).
    pub fn duration(&self, b0: Beats, b1: Beats) -> Seconds;
    pub fn base_bpm(&self) -> Bpm;
    pub fn trim(&self) -> f64;
    pub fn catch_up_open(&self) -> bool;
    /// Seed point (D6): the segment free-wheels until the next fresh sample sets it.
    pub fn reseed(&mut self);
    /// Seek under external authority: segment at (t, beat), integral kept.
    pub fn seek_to(&mut self, t: Seconds, beat: Beats, bpm: Bpm);
    /// Bumpless switch to another source (D6).
    pub fn hand_over(&mut self, profile: FollowProfile);
    /// After this tick's advance; `t` is the shown frame's grid time. Non-finite ignored (D18).
    pub fn observe(&mut self, t: Seconds, sample: ExternalSample) -> FollowEvent;
}
```

Engine side, section 3.2 (PlaybackEngine seam):
- `tempo: TempoSource { Map { beat_offset: f64 }, Follow(ExternalFollower) }` is the one converter (D8).
- `clip_start_times: AHashMap<ClipId, Seconds>` holds the latched start times, reserved at load. `get_clip_start_time_seconds` returns the latch for an active clip. Durations go through `converter.duration`.
- `set_external_sample(ExternalSample)` stores this tick's sample. Every placement drops it, as the stopgap's pending target was dropped. P2's `placement_after_an_external_target_wins_the_tick` is rewritten against the sample and keeps guarding that.
- `set_follow_authority(Option<FollowProfile>)` swaps the converter on an authority change. `Some` from Internal builds `starting_at` the current frame, at the beat with any hand-back offset included. `Some` from another source calls `hand_over`. `None` hands back with `Map { beat_offset: beat − map(time) }` (D8).
- `seek_external(beat)` is the seek under an external authority, called by the arbiter and on `FollowEvent::Seek` (D6).

Outside the follower, in `midi_clock_sync.rs`, sit the D20 estimator (`TickFilter`: fixed DLL, guards, refit, Link period while latched) and the D21 guard (`LinkTempoGuard`: a fixed 512-entry ring of tick arrivals, enough for 4 s at 300 BPM). In `content_thread.rs` sits the D19 calibration (`SyncCalibration`: the present-delay median over a fixed 120-entry ring, the display read, the per-source offset learners).

The tempting wrong turns:
- holding or skipping a frame "just this once" for a big error (that is a seek, D6);
- snapping the beat back to the source without the seek path;
- putting the phase loop inside a source controller (the follower owns phase; only D20's estimator lives in a controller);
- trimming time instead of the beat (D1);
- converting a clip's start beat through the segment each frame (D8).

The tempting wrong turn from P1 still applies: you will want to keep `current_time_double` and "snap it" after each `+= dt`. No. That is still an accumulator. The index is the state; time is computed from it.

### 3.2 `PlaybackEngine` seam (old → new)

```rust
// OLD
pub struct TickContext { pub dt_seconds: Seconds, pub realtime_now: Seconds,
    pub pre_render_dt: Seconds, pub frame_count: u64, pub export_fixed_dt: Seconds }
// NEW
pub struct TickContext {
    /// Content-tick grid frames since the last tick (D2). 0 only on export frame 0.
    pub frames: u32,
    pub realtime_now: Seconds,
    pub frame_count: u64,
}
```

| Old | New |
|---|---|
| fields `current_time_double`, `current_time`, `playback_speed`, `export_origin_seconds`, `on_time_changed` | `clock: TransportClock`; `tempo: TempoSource` (map, or the `ExternalFollower` in P3, section 3.1b (ExternalFollower)); `current_beat: f64` stays a cache, refreshed after every clock change from `beat_sample_time()`, beside `frame_beat` from `time()` for recording actions (D14) |
| `advance_time(dt)`, inherent `nudge_time(t)` | deleted; `tick_playing` calls `clock.advance(ctx.frames)` |
| `set_time(t)` | `locate(t)` |
| `seek_to(t)` | unchanged contract; `clock.seek_playing(t)` while playing, else `clock.locate(t)` |
| `set_beat(b)` + `sync_time_from_beat()` | P2: `follow_external_beat(b)`, marked stopgap: nearest frame on a frozen grid, hysteresis on a running one (section 6 (Phasing), P2). P3: deleted; sources hand `set_external_sample` an `ExternalSample` (D7) |
| `set_export_origin(t)` + `export_fixed_dt` | `begin_export_run(fps, start, warm_up)`: the export clock before the first seek, then seek, Play, warm-up, re-seek and frame numbering; `export_frame_context(k)` per frame; `export_restore_point()` and `end_export_run()` around it. Production and the tests run this one sequence. In export mode the tick calls `set_export_frame` whatever the sync flag |
| `external_time_sync` and its setters (engine and `SyncArbiterTarget`) | P2 keeps them as the stopgap; deleted in P3. The arbiter's `nudge_time` is deleted: the content thread takes one sample from the authority's own source (D7), so the authority match is the gate. The arbiter's `seek` stays gated and calls `seek_external` under an external authority |
| — | `transport_frame() -> i64`, `tick_dt() -> Seconds` (D5), `frame_rate() -> f32` (D10), `rederive_beat()` |
| each tick, outside export | `if project fps != clock.fps() { clock.set_fps(project fps) }` (D10) |

`tick_dt()` is what `render_content` reads for every graph (it takes no dt argument, so no caller can pass wall dt), and what `filter_ready_clips`, modulation and audio triggers read in place of `ctx.dt_seconds` / `ctx.pre_render_dt`.

### 3.3 `FrameTimer` — `crates/manifold-app/src/frame_timer.rs`

```rust
pub struct TickFrames { pub frames: u32, pub wall_dt: Seconds }

impl FrameTimer {
    pub fn new(target_fps: f64) -> Self;      // free-running, UI thread, unchanged
    pub fn new_grid(target_fps: f64) -> Self; // content thread
    pub fn consume_frames_at(&mut self, now: Instant) -> TickFrames;
    pub fn consume_tick(&mut self) -> f64;    // UI thread, unchanged
}
```

`new_grid` holds a `FrameGrid` (`crates/manifold-playback/src/frame_grid.rs`, built in P1), and `consume_frames_at` returns its `consume_at` count. `deadline(n) = anchor + Duration::from_secs_f64(n as f64 × step)`. `consume_at` advances `n` while `deadline(n + 1) <= now` (D2). After a stall it starts the search from an estimate and corrects by comparison in both directions. `frames = n − n_last`, at least 1, because production waits for `deadline(n_last + 1)`. `frame_clocked` gives `frames = 1`. `missed_ticks` becomes `frames − 1`. `resume_after_load` and `set_target_fps` re-anchor at now. Wall telemetry is unchanged.

Consequence: after a tick that is late by less than a frame, the next deadline is closer than a full period, so two content frames go out back to back. That is how fixed-step loops behave. The sim's late-frame rule (LIVE_SIM_CLOCK_DESIGN.md section 8 (Resolved decisions and deferred scope)) may see one extra late frame after a hitch. The P2 gate measures it.

### 3.4 Content thread

- `tick_frame(state_tx)` becomes `tick_frame_at(Instant::now(), state_tx)`. `tick_frame_at` is the seam the app-level test drives (section 5). It calls `consume_frames_at(now)` and builds `TickContext { frames, realtime_now, frame_count }`, then calls `render_content`, which reads `engine.tick_dt()` itself.
- `ContentThread` builds its timer with `FrameTimer::new_grid` (`app.rs:2556`, `headless_harness.rs:92`).
- After each engine tick: `if self.timer.target_fps() != self.engine.frame_rate() { self.timer.set_target_fps(…) }` (D10). The copies at `content_commands.rs:1358`, `:1436`, `:1492` and `:1656` are deleted. `rt_capture.rs:483`, `:498` write `settings.frame_rate` on the project before load instead.
- `derive_external_beat` (`content_thread.rs:1967-2015` on f32e3c5fd): P2 calls `follow_external_beat`. P3 replaces it with `external_sample(authority)`: it computes `R = tick_deadline + L` (D19), adds the source's learned offset for a non-Link source, asks only the authority's source for its `ExternalSample` at that instant (D7), and hands it to `engine.set_external_sample`. The existing gates stay in front of it (D6, free-wheel). While Link and another source both deliver, it also reads Link at the same instant for the offset learner (D19).
- `link_sample_at` (`content_thread.rs:589`) becomes `read_at = tick_deadline + L`; Link's `update` and the offset both use it (D15).
- On an authority change, `tick_sync_controllers` calls `engine.set_follow_authority` with the new source's profile, or `None` for Internal (D6, D8). Live tempo is cleared at the top of `tick_sync_controllers` (D21).
- Play aligned to MIDI clock (`content_commands.rs:894-911`): P2 `engine.locate(map(clock beat))`. P3 deletes the locate: the follower seeds on the first tick at the armed position (D20, D22).
- Calibration (D19): `ContentCommand::FramePresented { frame, at }` from the output drawable's presented handler feeds the present-delay median; `ContentCommand::SetSyncCalibration { extra_delay, offsets }` carries the stored prefs at startup; learned offsets go back to the UI in `ContentState` for saving. A follower seek (`FollowEvent::Seek`) goes through the arbiter's seek path for the current authority.

### 3.5 External sources (P3)

Anchors on f32e3c5fd.

| Source | Today | After |
|---|---|---|
| MIDI clock | Packed atomic state, no timestamps (`midi_clock_sync.rs:19-78`, `:134`); `set_external_time_sync` (`:586-597`); nudge or seek (`:779-796`); per-frame beat copy (`content_thread.rs:1989-2009`); BPM by windowed EMA (`midi_clock_sync.rs:669-733`) | Timestamped events and MIDI 1.0 position rules (D20). `sample_at(R)` from the fixed tick filter with its refit, `FAST` profile (D6, D7, D20). Play and pause unchanged. Seek unchanged (2 s, tick-delta range); the nudge branch and the beat copy are deleted. BPM from the estimator; Link's tempo while the guard is latched (D21). |
| Link | Beat at the tick deadline, followed past ¾ frame (`content_thread.rs:1973-1988`, `engine.rs:1182-1188`) | `sample_at(R)`: exact beat minus the offset, `FAST` profile, fresh every tick, while today's gates hold. Offset per D15. |
| OSC timecode | `set_external_time_sync` (`osc_sync.rs:371-373`); nudge or seek (`:405-440`); stamped at drain | Arrival stamped on the receive thread; `f64` seconds; half-frame bias on H:M:S:F; `sample_at(R)` through the map, `SLOW` profile (D7). Seek unchanged (0.5 s); the nudge branch is deleted. |
| AbletonOSC relay | NudgeBeats or SeekBeats actions per report (`transport_sync.rs:302-324`, `content_thread.rs:1933-1954`); stamped at drain (`ableton_bridge.rs:641-662`) | Arrival stamped on the receive thread; The report projected to `R` at Live's tempo, `SLOW` profile (D7, D8). SeekBeats unchanged (2 s); NudgeBeats is deleted. |
| Arbiter | `SyncArbiterTarget::nudge_time`, `set_external_time_sync` (`sync.rs:23-27`, `:181-228`) | Both deleted. Samples come only from the authority's own source, so the authority match is the gate. `seek` keeps its gate and calls `seek_external`. The gate-matrix tests for the two deleted calls (`sync.rs:318-357`) are deleted; the play, pause and seek matrices stay. |

The sources' own seek thresholds (2 s for CLK, 0.5 s for timecode) compare against `SyncTarget::position_time`, the map time of the beat on screen, not the grid time: under the follower the grid time drifts from the map with Live's tempo (about 4 s a minute at 128 over a 120 map) and would trip the 2 s seek, while the beat does not drift.

### 3.6 Edge cases

| Case | Behaviour after |
|---|---|
| Play / resume | Snap forward from the current time, then advance normally. No repeated frame. Under an external authority the follower seeds (D6). |
| Pause | Index kept. Follower cleared under an external authority. |
| Stop | Index 0. Follower cleared. |
| Seek / scrub while stopped | Coalesced as today (`content_thread.rs:327-400`). Snap forward. The playhead moves at most one frame from the click. |
| Seek while playing | Snap forward, hold one advance: the target frame is shown. |
| External seek (P3) | Nearest frame to the target's map time, no hold, epoch bumped, follower seeded at the source's beat (D4, D6). |
| Follower seek (P3) | An error past 50 ms held for 100 ms (fast) or 250 ms (slow), or past 150 ms inside a catch-up window, runs the external seek above. There is no snap (D6). |
| Sudden tempo change under an external source (P3) | Link: its beat taken on that frame. Others: the catch-up window opens; the beat may run up to half the change away from the new tempo for about a second, monotonic, no backward frame (D6, D20, D22). |
| Loop / wrap | No transport loop region exists. Session loops resolve from the beat (`engine.rs:1854-1890`), clip loops from video time. A future transport loop wraps as a seek. A loop wrap in Live: D22. |
| Punch / count-in | None exist. Automation gesture punch-in/out samples the grid beat (`automation.rs:139-178`). |
| Session launch quantise | Activates on the first frame whose sampled beat is at or after the target (`engine.rs:1907-1980`). D14 makes on-frame targets land on their frame. Under an external authority the targets convert through the segment (D8). |
| MIDI phantom clips | NoteOn runs before the engine tick (`content_thread.rs:604-605`) and stamps the previous frame's grid beat (`live_clip_manager.rs:462`, `:531`). The 5 ms NoteOff guard stays wall (`:761`). |
| Record / arm | Tempo, automation and MIDI recording stamp grid beats and times. |
| Undo during play | Arrangement edits reconcile next tick. Tempo edits follow D11. fps edits follow D10 through the per-tick compare. |
| Project load | `initialize` resets the clock; the timer re-anchors (`content_commands.rs:255`). The Link offset clears (D15). |
| Export start / end | D9: export clock first, then seeks, warm-up and frames on the export grid. The follower is suspended (D8). |
| Export under CLK or timecode | Advances on map beats (D9). Today it freezes. |
| Authority change | Converter swapped: from Internal the follower starts at the current frame's beat; between sources the handover is bumpless; back to Internal time and the beat are both kept, with a beat offset until the next Stop, seek, Play or tempo edit (D6, D8). Latched clip starts survive all three. |
| Output latency or a learned offset changes while playing | Adopted only when it moves by more than 1 ms. The next sample reads at the new instant; the phase error steps by the change and the trim removes it, or a lasting step past 50 ms seeks (D6, D19). |
| Export end under an external authority | A seed point: the follower resumes at the source's beat (D6, D9). |

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Transport time is `index × step`, never accumulated | `transport_clock::tests::time_is_absolute_at_two_hours` (60, 24, f32 29.97, bit-equal to the formula up to frame 432,000); negative gate `rg -n 'current_time_double \+=|fn advance_time' crates/manifold-playback/src` = 0 |
| On-frame edges land on their frame | `transport_clock::tests::on_frame_edges_land_on_their_frame`: every 1/16-beat edge at 120–180 BPM and 60/50/30/25/24 fps, first 2 hours, through `TempoMapConverter`. The frame whose `beat_sample_time` beat first reaches the edge equals the exact rational edge frame, and off-frame edges are unmoved. It must fail on today's sampling at 150 BPM. Engine: `frame_exact_transport::on_frame_edges_below_their_beat_start_on_their_frame` (150 BPM, 60 fps, edges whose frame time computes below their beat start their clip on that frame). Actions on a frame record its exact beat: `unquantised_loops_restart_on_whole_frames` (120 BPM 2 beats and 150 BPM ½ beat, 100 loops, every interval whole), `engine::tests::recording_boundaries_capture_last_touch_and_commit_once` (take at the exact beat, read back at the sampled one). |
| Seeks snap forward and round-trip, sampled beats included | `transport_clock::tests::seek_snaps_forward_and_round_trips`, `sampled_beats_round_trip_to_their_frame` (nine rates, 120/128/141/150/174 BPM); `frame_exact_transport::export_restores_the_frame_it_left` (same five tempos, playing and paused, zero moved) |
| Only a seek while playing holds | `transport_clock::tests::only_playing_seek_holds` (play, resume, paused seek, playing seek, external locate) |
| Same arrangement, same values per index, whatever the press phase, jitter or stalls | `frame_exact_transport::same_values_per_index_across_phase_jitter_and_stalls` (playback, CPU) and `frame_exact_app::same_values_per_index_through_content_thread` (app, one queued GPU run). Both committed red first (section 5). |
| Sub-frame jitter never changes `frames`; a missed deadline skips | `frame_grid::tests::grid_frames_ignore_subframe_jitter`, `grid_counts_missed_deadlines`, `grid_count_matches_deadline_comparison` (P1); `frame_timer::tests::frame_clocked_counts_one` (P2) |
| The tick grid and the clock share one step | `frame_grid::tests::grid_and_clock_share_the_step`: both checked against D1's written formula and the exact `n / fps`, not against each other |
| Export samples the same grid as live | `transport_clock::tests::export_frames_sit_on_the_song_grid` (P1, D17); `frame_exact_transport::export_frames_match_live_indices` (bit-equal to live at the project rate; at another rate the time bits match and the beat is within D14's offset) and `export_clip_inside_first_frame` (24 fps export of a 60 fps project, a clip starting between the export start and the next live-grid frame: no start before its own export frame), through `begin_export_run`, the sequence `content_export` runs |
| An export frame leaves no seek hold | `transport_clock::tests::export_frame_clears_a_pending_hold` |
| Export advances under external sync | `frame_exact_transport::export_under_external_sync_advances` |
| Every render consumer gets `frames × step` (D5) | `frame_exact_transport::tick_dt_reaches_consumers_as_whole_frames` (`pre_render`, playing and paused); `frame_exact_app::same_values_per_index_through_content_thread` asserts the dt `render_content` hands graphs each tick; `render_content` takes no dt argument |
| P2 only: the external stopgap neither holds nor skips on jitter or, under Link, a late wake, and never undoes a placement (STOPGAP(frame-exact P3)) | P2's `frame_exact_transport::external_follow_never_holds_or_skips_without_drift`, `external_follow_corrects_a_real_frame_of_drift`, `link_read_at_the_deadline_ignores_late_wakes` and `transport_clock::tests::follow_external_keeps_the_grid_inside_three_quarters_of_a_frame` are deleted in P3 with the stopgap. `placement_after_an_external_target_wins_the_tick` is rewritten against `set_external_sample`. `link_sync::tests::link_is_read_at_the_given_instant` and the `frame_grid` deadline tests stay. |
| fps has one source and one sanitising rule | `transport_clock::tests::fps_outside_one_to_a_thousand_falls_back_to_sixty`, `frame_grid::tests::bad_fps_falls_back_without_panicking` (P1); `content_thread::tests::timer_and_clock_follow_project_fps` (load, SetFrameRate, undo, redo) |
| Bad input never moves or breaks the clock (D18) | `transport_clock::tests::non_finite_and_absurd_input_is_safe` (NaN, ±inf, ±1e300 to every mutator; `i64` extremes to `set_export_frame`; `u32::MAX` advances) |
| Under an external authority the index advances exactly `frames`; the beat is continuous, strictly increasing between seeks, and within D7's phase and speed bounds of the source (P3) | `external_follow` P3 proofs in section 5 (Proof plan): `link_follows_with_steady_beat_speed`, `clk_follows_without_stair_steps`, `abletonosc_fallback_stays_within_bound`, `timecode_follows_song_seconds`; every P3 proof counts frames where the beat goes backward outside a seek and asserts zero |
| The loop matches its stated constants: one overshoot and no ringing, D6's settle times, trim never past the cap outside a catch-up window, no wind-up | `external_follow::tests::step_response_matches_the_profile` (2, 5, 10, 20, −20 and 40 ms steps, both rows, settle within D6's times +20%, overshoot under 0.6 ms), `trim_never_exceeds_the_cap_outside_catch_up`, `integral_freezes_at_the_cap` (2 s at the cap, then overshoot under 0.6 ms) |
| Two outcomes only: a lasting error seeks, a short one is trimmed, a single outlier moves the beat by at most the cap | `external_follow::tests::lasting_error_seeks_after_the_hold`, `single_outlier_never_seeks`; `tests/external_follow.rs::relocate_seeks_once`; negative gate `rg -n 'Snapped|snap_' crates/manifold-playback/src/external_follow.rs` = 0 |
| The catch-up window opens only on a confirmed tempo change, and never in steady play | `external_follow::tests::catch_up_opens_on_a_tempo_move_only`, `catch_up_cap_is_half_the_change_at_most_a_quarter`; `tests/external_follow.rs::no_false_seeks_or_catch_up_in_an_hour` |
| Seeds, handovers and hand-back keep the beat, and hand-back keeps time | `external_follow::tests::seed_takes_the_source_beat`, `handover_is_bumpless`, `link_tempo_change_takes_the_link_beat`; `tests/external_follow.rs::handover_keeps_the_beat_continuous`, `hand_back_keeps_time_and_beat` |
| No distant beat is converted to time through the segment; video elapsed never rescales (D8) | `tests/external_follow.rs::video_elapsed_survives_tempo_changes`; the `time_at` debug assertion; `engine::tests::durations_use_beat_differences` |
| Tempo changes are followed through the base tempo, not the trim | `tests/external_follow.rs::clk_tempo_change_follows_through_the_estimate` (D20's table as bounds), `abletonosc_tempo_change_with_listener_lag` |
| MIDI clock follows MIDI 1.0 position rules | `midi_clock_sync::tests::first_clock_after_start_is_the_start_position`, `ticks_while_stopped_do_not_move_the_position`, `song_position_then_continue_arms_the_position` |
| The tick filter ignores single late ticks, refits on real changes and survives a stall | `midi_clock_sync::tests::filter_ignores_a_single_late_tick`, `refit_needs_three_growing_misses`, `stall_then_burst_does_not_diverge` (150–600 ms, stamped on arrival: period inside the 20–300 BPM clamp, phase under 20 ms, no seek); the existing `bpm_estimator_survives_backward_tick_jump` (`midi_clock_sync.rs:957`) kept against the new filter |
| The Link guard holds through tempo changes and rejects a session Live is not in (D21) | `midi_clock_sync::tests::guard_latches_and_holds_through_tempo_changes`, `guard_rejects_a_foreign_link_session` (Link 127 and 127.8 against Live 128) |
| The Link offset is taken on every start, quantum-aligned on a Link start (D15) | `tests/external_follow.rs::link_started_play_aligns_to_the_bar_and_steers`, for BUG-0u1al (Link-initiated Play never caches the Link beat offset) |
| Live tempo never outlives its source and never writes the map (D21) | `tests/external_follow.rs::peers_vanishing_keeps_the_beat_and_the_map`, for BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point); negative gate `rg -n 'add_or_replace_point\(Beats::ZERO' crates/manifold-app/src/content_thread.rs` = 0 |
| Every external source is read at `deadline + L` plus its learned offset; `L` and offsets move only by measurement (D19) | `tests/external_follow.rs::read_instant_follows_the_measured_latency`; `sync_calibration::tests::present_delay_is_a_median_with_hysteresis`, `offset_is_learned_against_link_and_bounded`, `bad_extra_delay_loads_as_zero`; `content_thread` asserts the read instant in `frame_exact_app::same_values_per_index_through_content_thread` |
| Bad samples never break the follower (D18) | `external_follow::tests::non_finite_samples_are_ignored` (NaN and ±inf beats, tempos of 0, −1, NaN, 1e300) |
| External sources cannot stop the grid | negative gate `rg -n 'external_time_sync|follow_external|defer_external|PhaseLock|enum Correction|NudgeBeats' crates` = 0 (P3) |
| Video samples, never integrates | negative gate `rg -n 'time_accumulator|playback_time \+=' crates/manifold-media/src` = 0; `video_renderer::tests::target_frame_follows_transport_time` (P4) |

## 5. Proof plan

Reuse, don't build:
- the engine tests' `create_engine` / `StubRenderer` / `load_project` helpers (`crates/manifold-playback/tests/engine_tick.rs:6-120`);
- the live-versus-export pattern in `tests/param_step_clip_edge.rs:200-219`;
- `FrameTimer::consume_frames_at` for injected instants (`frame_timer.rs`);
- `manifold frame-time` for pacing;
- `headless_content_thread` for app-level checks.

**Fail first.** The first commit of P2 adds both determinism tests against today's `TickContext` and today's `tick_frame`, and records them red in the commit message.
- The playback test drives the engine through one test helper, `drive(engine, instants)`. Today the helper turns consecutive instants into `dt_seconds`. After P2 it turns them into `frames` with the D2 rule. The helper is the only part of the test that changes. The test body and its assertions stay as committed red.
- The app test needs `tick_frame_at` (section 3.4). That seam is the commit's only production change and moves no behaviour.

**Playback test** — `crates/manifold-playback/tests/frame_exact_transport.rs::same_values_per_index_across_phase_jitter_and_stalls`:
1. Project:
   - 150 BPM on beats 0–8, 128 BPM to beat 32 (frame 867), then 141;
   - an automation lane with curve and step segments, with steps on on-frame beats;
   - a sync LFO (1/16 sine) and a free LFO;
   - a clip-edge step envelope;
   - clips starting on off-grid beats and on on-frame beats (at 150 BPM every 1/8 beat is a 60 fps frame);
   - a session launch quantised to 4 beats.
2. Run A: Play right after the last stopped tick, 1200 ticks one step apart (the app test runs 960). 900 ticks at 128 BPM from beat 0 would stop exactly at beat 32 and never play the 141 section.
3. Run B, seeded:
   - Play pressed at a random phase within a frame;
   - each tick's instant jittered by ±40% of a frame;
   - one 3-frame stall.
4. Record per transport index: `time` bits, `beat` bits, active clip ids, every param `value` bit, session slot state.
5. For every index both runs rendered, the records are equal bit for bit. It was red before P2 because each tick added jittered wall dt. The press phase only decides which tick is the first playing one: dt ran from the previous tick, and Play read no clock.

**App test** — `crates/manifold-app/src/frame_exact_app.rs::same_values_per_index_through_content_thread`. Same shape through `headless_content_thread` and `tick_frame_at` with the real grid timer, not `frame_clocked`: random press phase, jitter and a 3-frame stall. It compares per-index beat bits and param bits from the engine and does not compare pixels. One queued GPU run (audit item 1.8).

**L2 artifact:** `manifold frame-time "<Liveschool Live Show V6 LEDS.manifold>" --frames 3600 --transport-log <path> --pre-play-delay-seed <n>`, twice with different seeds. The seed waits a random 0–2 frames of wall time before Play, so the two runs start at different phases. This extends the existing tool; it adds no new one. Each log line is `index beat_bits automation_hash`. `diff` over shared indices must be empty. Also report the `frames` histogram and the count of ticks with `frames > 1`.

**Edge cases:** one test each in `frame_exact_transport.rs`:
- `tempo_edit_during_play_keeps_beat`
- `fps_change_remaps_forward`
- `stop_resets_pause_keeps`
- `session_launch_on_first_grid_frame`
- `noteon_stamps_previous_grid_beat`
- `stall_skips_and_stays_in_time`
- `seek_while_playing_renders_target_first`
- `play_advances_from_anchor_without_repeat`
- `hand_back_keeps_time_and_beat` and `video_elapsed_survives_tempo_changes` (D8, P3; listed with the P3 proofs below)

**Export regression:** the export tests above, plus the existing ones untouched: the `param_step_clip_edge.rs` export arm, `crates/manifold-app/src/rt_dynamic_export_tests.rs`, and the scoped GPU proofs `scripts/gpu_scope.py` maps for `content_export.rs`.

### P3 proofs

All CPU and seeded. Engine-level tests live in `crates/manifold-playback/tests/external_follow.rs`, with the source models in `tests/support/sync_sources.rs`. Unit tests sit next to their code; section 4 (Invariants & enforcement) lists them.

**Source models.** One per D7 noise model, seeded with the SplitMix generator of `transport_clock/tests.rs:8-20`:
- Link: an exact beat at any instant, a 50 ppm rate gap, ±0.2 ms timeline steps every 30 s, and tempo jumps and ramps.
- MIDI clock: 24 PPQN arrival times from an integrated tempo curve (steps and ramps), ±1 ms uniform plus 0.3 ms Gaussian jitter, 0.5% of ticks 4 ms late, an 80 ppm gap, and optional delivery stalls released as a burst stamped on arrival.
- AbletonOSC: 10 Hz ± 20 ms reports, each ±10 ms off the true position, ±1 ms network jitter, a known RTT, and a tempo listener that reports Live's tempo 0, 100 or 200 ms late.
- Timecode: song seconds on a free-running metro, with or without 25 fps frame labels, up to 1 ms arrival jitter.
- A Link session at a tempo Live is not part of, for the guard.

Each model produces raw arrivals (ticks with instants, packets with instants) and feeds them to the real code: D20's filter and guard, the real AbletonOSC and timecode projections, the real Link offset logic, the real calibration. The model never stands in for the code under test. Phase error is measured against the model's true beat at the read instant.

**Fail first.** P3's commit C2 (section 6 (Phasing), P3) adds these tests against the stopgap and records each red with its measured failure. One helper changes between red and green, as in P2: `feed(engine, sample)`. On the stopgap it does what the content thread does today: `follow_external_beat(beat)`, with `set_external_time_sync(true)` for CLK and timecode. After P3 it calls `set_external_sample`. The engine is driven by P2's `drive` helper with seeded wake jitter (`tests/frame_exact_transport.rs:249-274`). Tests 9 and 10 drive the content thread's tempo-plane code through `headless_content_thread`, so they also fail first on real code.

**Measured each frame after a 5 s warm-up:** phase error in ms; beat speed, as the shown beat's increment against the music's increment over the same frame; the speed change from the previous frame; frames where the beat goes backward outside a seek (always zero); seeks, from the follower's events (on the stopgap: frames whose index moved by anything other than `frames`). Bounds are D7's "test bound" columns unless a test says otherwise.

| # | Test | What it asserts | Why it fails on the stopgap |
|---|---|---|---|
| 1 | `link_follows_with_steady_beat_speed` | Map 120 BPM, Link 128 (they disagree), 10 simulated minutes: Link bounds, no seeks | The beat runs at the map's tempo and the ¾-frame rule moves the index about every 11 frames: speed changes by about 100% on those frames, phase up to 12.5 ms |
| 2 | `clk_follows_without_stair_steps` | 120 and 174 BPM, with Link latched and alone, 10 minutes: CLK bounds | The frozen grid jumps to the last tick: beat steps of 0, 1 or 2 frames' worth, phase up to one tick (20.8 ms at 120) |
| 3 | `clk_tempo_change_follows_through_the_estimate` | D20's tempo-change table, nine seeded runs each, bounds at twice the simulated peak (Link latched: 140 → 70 ≤ 20 ms, others ≤ 8 ms; alone: 120 → 140 ≤ 25 ms, 128 → 174 ≤ 35 ms, 174 → 128 ≤ 55 ms, 140 → 70 ≤ 240 ms, ramps ≤ 20 ms). No seek, no backward frame. Outside the catch-up window the trim never passes 0.5% | Stair steps, as test 2 |
| 4 | `phase_step_settles_by_trim` | Link source steps by +5, +10, +20, −20 and +40 ms: no seek; under 1 ms within 1.0, 2.2, 4.6, 4.7 and 9.3 s; overshoot under 0.6 ms; speed within the cap | A 20 ms step lands as a whole-frame jump: the beat is not continuous |
| 5 | `relocate_seeks_once` | Fast row, Link: +40 ms is trimmed with no seek; +300 and −500 ms seek exactly once, 100 ms after the move, and the epoch bumps. Slow row, AbletonOSC: +300 and −500 ms (a one-beat Live loop at 120 BPM) seek once after 250 ms; +40 ms is trimmed | The index lands on the nearest frame, up to 8 ms off, and no epoch bump happens for Link |
| 6 | `abletonosc_fallback_stays_within_bound` | Live 128 against map 120, 10 minutes, listener lag 100 ms: AbletonOSC bounds, no seeks | Reports are stamped at drain (about 8 ms mean lag) and nudges past ¾ frame move the index |
| 7 | `timecode_follows_song_seconds` | Unquantised seconds at metros 40.0, 40.1, 33.3, 33.4, 20 and 50 ms, three start phases: the unquantised bounds. LiveMTC frame labels at the same metros: phase ≤ 21 ms, no seeks | The frozen grid is placed per message: holds, then multi-frame steps |
| 8 | `handover_keeps_the_beat_continuous` | CLK stops mid-play and AbletonOSC takes over (Live 128, map 120); Link to CLK; Internal to Link. On the handover frame the beat increment is within the cap of the previous one; no seek when the sources agree | After CLK hands to AbletonOSC the grid advances at the map's tempo and nudges move it in whole frames: speed and phase bounds fail |
| 9 | `link_started_play_aligns_to_the_bar_and_steers` | Stop; Link starts play at Link beat 37.3 with MANIFOLD paused at beat 10: the offset is a multiple of 4, MANIFOLD's beat matches Link's modulo 4, and Link steers for one minute within Link bounds. BUG-0u1al (Link-initiated Play never caches the Link beat offset) | The offset stays NaN, so Link never steers and the beat drifts at the map's tempo |
| 10 | `peers_vanishing_keeps_the_beat_and_the_map` | Link at 128 over a one-point map at 120 for 5 minutes; peers vanish: the map is unchanged (point count and tempo). BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point) | The stale 128 is written into the beat-0 point |
| 11 | `hand_back_keeps_time_and_beat` | Same set-up as 10: on the hand-back frame neither time nor the beat moves; the temporal reset does not fire; a free LFO and a latched video clip carry on; the beat then runs at 120. The next Play clears the offset with the beat kept | Time is re-derived from the map, so it jumps by about 20 s and the temporal reset fires |
| 12 | `video_elapsed_survives_tempo_changes` | A non-warped video clip started 30 s before a Link change from 120 to 140, and one started 120 s before: video elapsed time stays within one frame of the true elapsed time through the change. Ten minutes under LiveMTC timecode: the activation frame's offset inside the label phase bound (21 ms), then no more than 1 ms of drift | Under the stopgap the index is placed at the map time of Link's beat, so elapsed time runs at the map's tempo: 17% off at 140 over 120 |
| 13 | `guard_latches_and_holds_through_tempo_changes` | CLK with Live on Link: every D20 tempo jump keeps the guard latched. A Link session at 127 and at 127.8 BPM that Live is not part of: no seek in 3 minutes | No guard exists; the stopgap ignores Link's tempo under CLK |
| 14 | `clock_stall_then_burst_does_not_diverge` | CLK alone at 120, stalls of 150, 200, 300 and 600 ms released as a burst stamped on arrival: phase ≤ 30 ms, no seek, the period inside 20–300 BPM throughout | The burst moves the frozen grid by up to 600 ms in one frame |
| 15 | `abletonosc_tempo_change_with_listener_lag` | Listener lag 0, 100 and 200 ms; jumps 174 → 128, 128 → 174, 120 → 140, 140 → 70. Lag 0: peak ≤ 8 ms. Lag 100 ms: peak ≤ 80 ms (140 → 70: ≤ 230 ms), back within a frame after 3 s, no seek. Lag 200 ms: no bound; 140 → 70 may seek once by the catch-up rule (D6). No backward frame at any lag | Nudges move the index in whole frames, and Live's tempo is never followed |
| 16 | `read_instant_follows_the_measured_latency` | Presented times 25 ms after each deadline with ±3 ms jitter: the read instant settles on 25 ms within 3 s and then never moves on jitter. A CLK source 7 ms late against Link: the learned offset converges to 7 ms within 40 s and the CLK beat then matches Link's within 1 ms. With `MANIFOLD_ExtraOutputDelayMs` at 10: the read instant is 35 ms after the deadline | No measurement exists, and the stopgap shows the source one frame ahead: 16.7 ms off at 60 fps |

**One-hour runs, follower only.** `tests/external_follow.rs::no_false_seeks_or_catch_up_in_an_hour`: each source model drives the real filter, guard and follower directly (no engine) for one simulated hour at 60 fps, steady tempo: zero seeks, zero catch-up opens after warm-up, D7's bounds throughout. The engine-level tests above use 10 simulated minutes to keep the suite fast.

**Regression guards, not fail-first:** `export_under_external_sync_advances` extended to assert map beats (D8, D9) and that export end seeds the follower; `placement_after_an_external_target_wins_the_tick` rewritten against `set_external_sample`; the arbiter's play, pause and seek gate matrices (`sync.rs`).

**App level, one queued GPU run:** `frame_exact_app::same_values_per_index_through_content_thread` asserts the content thread reads at `tick_deadline + L` and that `FramePresented` reaches the calibration.

**Rig log.** C0 lands it on main before any constant is frozen (section 6 (Phasing), P3); C10 keeps it as the operating log. One info line per seed, seek, catch-up window, guard latch or unlatch, adopted offset and handover, for example `[Transport] CLK seek −312 ms` and `[Transport] guard: Link tempo latched for CLK`. While an external source drives, one line every 5 s: `[Transport] CLK lock: phase +0.4 ms, trim +0.03%, base 128.00 BPM, L 24.8 ms (present 24.8, display 0, extra 0), CLK offset +6.1 ms`.

As built (C10): `sync_log.rs` writes the operating lines from the follower's per-tick event (`FollowEvent::Seek` carries its error) and state, after the engine's tick. Guard and calibration lines moved under the `[Transport]` prefix. C0's rig log files (`sync-*` beside the session log) stay as they are, for the raw arrivals.

## 6. Phasing

P1 and P2 land together as one merge. That merge keeps today's external-sync behaviour as a named stopgap. Peter then rig-tests it. P3 lands next. P4 lands separately. Repeatable video frames are not promised before P4.

### P1 — The clock type, CPU only

- **Entry:** branch `feat/frame-exact-clock` at or after 09a44cb4c; `rg -n 'struct TransportClock|transport_clock' crates` = 0.
- **Read-back:** sections 2 and 3.1; `crates/manifold-physics/src/clock.rs:278-400`; `driver.rs:167-200`. Restate D1, D4, D6, D8 and D14 and the forbidden moves.
- **Deliverables:**
  - `crates/manifold-playback/src/transport_clock.rs` with the section 3.1 (TransportClock) API, private `PhaseLock` and `LiveTempo` (P3 deletes the first and moves the second, D6);
  - `crates/manifold-playback/src/frame_grid.rs`, the D2 deadline grid that P2's `FrameTimer` wraps;
  - `pub mod transport_clock;` and `pub mod frame_grid;` in `crates/manifold-playback/src/lib.rs`;
  - the `transport_clock` tests named in section 4.
- **Gate:** `cargo nextest run -p manifold-playback transport_clock`; `cargo clippy -p manifold-playback -- -D warnings`. Negative: `rg -n 'Instant|SystemTime' crates/manifold-playback/src/transport_clock.rs` = 0.
- **Forbidden:** wiring into the engine; a rational fps type; an `f64` time field in the clock.
- **Size:** about 350 lines plus 350 of tests. **Demo:** none (L1). **Test scope:** manifold-playback.

### P2 — Live and export on the clock; external sources on a stopgap

- **Entry:** P1 committed. Re-run the `TickContext` inventory (expect 33), the writer commands in audit item 1.2 and the harness list in audit item 1.8. A different count means stop and list the new sites.
- **Read-back:** audit items 1.2–1.8 and sections 3.2–3.6; `content_thread.rs:567-653`; `content_export.rs:560-1022`; `frame_timer.rs` whole. Restate D2–D5, D9, D10, D14 and the stopgap below.
- **First commit:** the two determinism tests and `tick_frame_at`, red (section 5).
- **Seam brief, compiler-driven:** delete `TickContext.dt_seconds`, `pre_render_dt` and `export_fixed_dt`; the errors are the checklist.
  - Mechanical: `dt_seconds: Seconds(1.0 / 60.0)` becomes `frames: 1` (`tests/engine_tick.rs:111-117`).
  - Non-frame step `d`:
    - if `d × 60` is whole, use `frames = d × 60`;
    - otherwise set the test project's `settings.frame_rate = 1.0 / d` and use `frames = 1`;
    - examples: 0.005 → frame_rate 200; 1000.0 → 60,000 frames at 60 fps.
  - Export: `begin_export_clock` before `content_export.rs:717`; `frames` 0 for frame 0, then 1; the export branch moves out from under the sync flag.
  - `set_time` callers → `locate`; `seek_to` picks `seek_playing` or `locate` by state.
  - `ContentThread` timer → `FrameTimer::new_grid`; the timer follows `engine.frame_rate()`; the four copies and the rt-capture override go (D10).
  - `render_content(…, dt, …)` → `engine.tick_dt()`.
  - `angular.rs:207-211` moves to the grid frames either side of beat 1.
- **The stopgap, named:** each remaining external path carries `// STOPGAP(frame-exact P3)`; P3's entry counts them.
  - `external_time_sync` stays: while it is set, live playing ticks don't advance the clock, as today.
  - `set_beat` + `sync_time_from_beat` become `follow_external_beat`, and nudges go to the same place. On a frozen grid (CLK, timecode) or a stopped transport the index goes to the frame nearest the target at once.
  - On a grid that advances on its own (Link's beat copy every tick, AbletonOSC nudges per message), the target waits for the tick's advance and moves the index only when it is more than ¾ frame from it (`TransportClock::follow_external`, D6's threshold without persistence). Re-placing every tick turned sub-frame jitter into held and skipped frames: at a half-frame phase with ±0.02 frame of jitter, about 890 of each a minute at 120 BPM. The target is read as the tick begins and the tick shows the next frame, so it is compared one frame on, as the old place-then-advance did; a tick that counted two frames is not counted twice. The deferred target lives in the clock and every placement drops it (seek, locate, stop, load, fps change, export), so a seek drained after a nudge in the same tick wins. Link is read at the deadline of the frame the tick shows (`FrameTimer::tick_deadline`, D15), not when the tick woke, so a late wake does not read as Link running ahead. AbletonOSC nudges still carry the wake's lateness; P3 timestamps them.
  - So under CLK, timecode, AbletonOSC and Link, MANIFOLD follows the external source as it does today, within ¾ of a frame of today's value. Being exactly today's behaviour would need an off-grid time source next to the clock, and that would re-open the path this bead closes.
  - The determinism claim in P2 holds under Internal authority only.
- **Deliverables:** the above; `crates/manifold-playback/tests/frame_exact_transport.rs` and `crates/manifold-app/src/frame_exact_app.rs` (tests in section 4 and section 5, minus the P3 ones); the timer grid tests; deletion of `advance_time`, inherent `nudge_time`, `playback_speed`, `on_time_changed`; `--transport-log` and `--pre-play-delay-seed` on `frame-time`.
- **Gate:**
  - `cargo nextest run -p manifold-playback`;
  - `cargo nextest run -p manifold-app frame_timer content_thread content_export`;
  - clippy on both crates;
  - the app determinism test and the scoped GPU proofs for the export path, through `scripts/gpu_proofs_gate.py`;
  - the `journey-proofs` angular journey, one queued run;
  - a `MANIFOLD_RENDER_TRACE=1` paced `frame-time` run with no frame over 20 ms that main does not also show.
  - Negative: `rg -n 'fn advance_time|export_fixed_dt|dt_seconds|pre_render_dt|playback_speed|on_time_changed' crates` = 0, and `rg -n 'timer.set_target_fps' crates/manifold-app/src/content_commands.rs` = 0.
- **Measured numbers to report:** the L2 transport-log diff (empty), the `frames` histogram over 3600 ticks, the count of `frames > 1` ticks.
- **Forbidden:** a dt-driven advance for any caller; clamping `frames`; changing the UI timer; changing sync-controller behaviour beyond the stopgap.
- **Size:** about 500 lines changed plus 500 of tests and migrations. **Demo:** L2 transport-log diff, then L4: Peter plays the same section twice under Internal authority, and once under CLK to confirm nothing regressed. **Test scope:** playback and app, scoped.

### P3 — External sources steer the beat by tempo trim, read at the screen instant

- **Entry:** branch `feat/frame-exact-sync` at or after f32e3c5fd, with this revision's review answered. C0 and C1 land on main first, and the P3 branch merges main after them. Re-run `rg -n 'STOPGAP\(frame-exact P3\)' crates` (expect 23 in 9 files) and `rg -l 'external_time_sync|follow_external_beat' crates` (expect 9). A different count means stop and list the new sites. Re-run the D8 caller audit: `rg -n 'beat_to_timeline_time|get_clip_start_time_seconds|get_clip_end_time_seconds' crates -g '*.rs'`.
- **Read-back:** audit item 1.10, D6–D8, D11, D15, D18–D22, sections 3.1b and 3.4–3.6, the P3 proofs in section 5 (Proof plan); `midi_clock_sync.rs` whole; `link_sync.rs` whole; `osc_sync.rs:270-440`; `transport_sync.rs:1-340`; `ableton_bridge.rs:641-670`, `:874-915`, `:2420-2462`; `content_thread.rs:575-660`, `:1729-2160`; `content_commands.rs:870-975`; `engine.rs:2640-2760`, `:3119-3200`; `content_pipeline.rs:3325-3365`. Restate D6's loop, rows and catch-up rule, D7's read instant, D8's conversion rule, D20's filter and refit, D21's guard and the forbidden moves.
- **Commits.** Small, in this order. Each passes clippy `--tests` and scoped nextest on the crates it touches, except that C2's tests stay red until the commit named for them.
  0. **C0 — Rig log, instrumentation only. Lands on main first**, from its own lane branch, so Peter's rig produces a log before any constant is frozen. No behaviour changes. It logs:
     - raw MIDI clock bytes with midir's timestamp and an `Instant` taken in the callback (raw lines only with `MANIFOLD_SYNC_RAW=1`);
     - every 5 s, Link minus CLK and Link minus AbletonOSC phase at the same tick deadline;
     - each tempo-listener arrival against the change in AbletonOSC song-time slope, which measures the listener lag;
     - AbletonOSC report intervals and inter-arrival times;
     - on the output window, `presentedTime` minus the deadline (median and p95 every 5 s), which needs `GpuDrawable::on_presented` in `manifold-gpu`;
     - on each display change, `CVDisplayLinkGetOutputVideoLatency` and whether the IORegistry holds that display's raw EDID with an HDMI latency byte.
     Peter runs one rig session from main and sends the log; it checks D7's noise models, the listener lag and D19's reads before C3.
  1. **C1 — BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point). Lands on main early.** Delete the Internal beat-0 write (`content_thread.rs:2131-2151`); live tempo lasts one tick (D21). Test 10 is written red first and recorded in the commit message. Safe on the stopgap, which already places the index at the map time of Link's beat.
  2. **C2 — Red tests.** `tests/external_follow.rs` and `tests/support/sync_sources.rs` with tests 1–9 and 11–16 and the `feed` helper on the stopgap. The commit message lists each test red with its measured failure.
  3. **C3 — The follower.** `external_follow.rs` (section 3.1b (ExternalFollower)) with its unit tests. `LiveTempo` moves in from `transport_clock.rs`. Not wired yet.
  4. **C4 — Distance-safe conversion, before any engine wiring.** `TempoSource` with `Map { beat_offset }`, latched clip start times, `converter.duration` for durations and loop lengths, the recently-started gate through the converter (`engine.rs:3188-3198`), and the `time_at` debug assertion (D8). Under the stopgap nothing visible changes: the latch equals today's map value and the offset is 0. Existing tests stay green.
  5. **C5 — Engine on the follower; the stopgap deleted.** `set_external_sample`, `set_follow_authority` (hand-back with a beat offset), `seek_external` and `FollowEvent::Seek`, export end as a seed point. Deleted: `external_time_sync` and its setters, `follow_external_beat`, `follow_external_time`, `follow_external`, `defer_external`, `take_pending_external`, `PhaseLock`, `Correction`, `Smoothing`, `observe_external`, `clear_external`, the arbiter's `nudge_time` and `set_external_time_sync`, their mocks (`osc_timecode.rs:73-88`, `midi_clock_sync.rs:1010-1030`, `osc_sync.rs:622-650`, `sync.rs:245-270`) and the P2 stopgap tests (section 4). `tests/trigger_source_switch.rs` moves to `set_follow_authority`. The content thread hands over today's source values as samples, stamped as today. `feed` flips. Tests 1, 4, 5, 8, 11 and 12 turn green.
  6. **C6 — MIDI clock.** D20: timestamped events through `mpsc` with CoreMIDI packet times, MIDI 1.0 position rules, the fixed tick filter with its guards and refit, `sample_at(R)`. The packed atomic state, the transport-time integrator and the old BPM estimator are deleted. Tests 2, 14 and the CLK-alone half of 3 turn green.
  7. **C7 — AbletonOSC and timecode.** Arrival stamps on both receive threads, `f64` song seconds, the half-frame bias, `sample_at(R)`, Live's tempo for AbletonOSC. `NudgeBeats` and the timecode nudge branch are deleted. Tests 6, 7 and 15 turn green.
  8. **C8 — Link start and the tempo plane.** The decision code the content thread holds today (Link offset `content_thread.rs:2020-2034`, live-tempo bookkeeping `:1784-1828`) moves into a pure `external_sync.rs` in the same commit, no separate extraction. D15 offsets at `R` with quantum rounding on a Link start; D21's latched guard for CLK and AbletonOSC; Link's beat on a Link tempo change. Tests 9, 13 and the Link half of 3 turn green.
  9. **C9 — Output latency and calibration** (after C5: before it, the stopgap's one-frame lead hides the delay). D19: the presented handler feeding `L_present`, the display read, the per-source offset learners, the `MANIFOLD_ExtraOutputDelayMs` override, the `ContentState` and `UserPrefs` round trip for learned offsets. Test 16 turns green.
  10. **C10 — Logs and gates.** C0's lines become the operating log (section 5 (Proof plan), rig log), the app-level read-instant assertion, the negative gates, the one-hour runs, this doc's status line.
- **Gate:**
  - `cargo nextest run -p manifold-playback`;
  - `cargo nextest run -p manifold-app content_thread content_commands frame_timer`;
  - `cargo nextest run -p manifold-gpu` for the presented handler;
  - clippy `--tests` on the three crates;
  - `frame_exact_app::same_values_per_index_through_content_thread`, one queued GPU run through `scripts/gpu_proofs_gate.py`.
  - Negative: `rg -n 'STOPGAP\(frame-exact P3\)|external_time_sync|follow_external|defer_external|PhaseLock|enum Correction|NudgeBeats|fn sync_time_from_beat|fn set_beat\b|Snapped' crates` = 0.
- **Demo:** L4 on Peter's rig. The launch command for the branch build goes with the request.
  - MIDI clock with Link on, 5 minutes, including a tempo ramp and a scene with a tempo jump. The 5-second lock lines stay under 1 ms and no seek appears. LFO motion is smooth where it stair-stepped before.
  - The same with Link off, including a big tempo drop: the beat eases into the new tempo within a second, with no jump back (Q5).
  - Link: start from Ableton after a Stop. MANIFOLD follows, and its bar lines up with Ableton's.
  - Latency: read `L` and the learned offsets from the log, then film the flash and the click from D19 to check them. Nothing to set unless the display adds an unreported delay.
  - Kill the MIDI clock mid-play with AbletonOSC on: the handover line appears and the beat carries on without a visible jump.
  - Quit Live's Link mid-play: feedback, trails and LFOs carry on, and the project's tempo takes over.
  - The click-script goes in the landing merge commit.
- **Forbidden:** holding, skipping or snapping for phase; a backward move of the beat outside a seek; filtering phase inside a source controller (only D20's filter lives there); a beat in `TransportClock`; trimming time; converting a distant beat through the segment; re-tuning the 2 s and 0.5 s source seek thresholds; applying a per-frame latency measurement directly; a visible latency field; private display APIs; keeping any deleted stopgap path beside the follower.
- **Size:** about 650 lines changed plus 550 lines of tests and source models, C0 included.

### P4 — Video samples transport time

- **Entry:** P3 landed. Read `video_renderer.rs:500-900`, `engine.rs:2559-2700`, `:2746-2866`.
- **Deliverables:** D12. `ClipRenderer::pre_render` receives each playing clip's target source time from the engine. Both integrators are deleted. Test `target_frame_follows_transport_time`.
- **Gate:** `cargo nextest run -p manifold-media -p manifold-playback`; scoped GPU proofs for video export; the negative gate in section 4.
- **Demo:** L2. A 30 fps clip in a 60 fps project, played twice: the logged source frame per transport frame is identical.
- **Size:** about 150 lines plus 100 of tests.

### Landing sweep (each landing)

- Update this doc's status line.
- CORE_ENGINE_MAP.md section 4 (Time model & tempo), section 7 (External sync) and section 11 (The threshold table).
- The two stale CORE_ENGINE_MAP items in audit item 1.9.
- VSYNC_AND_FRAME_PACING.md, "Content Thread Pacing": add the grid deadline.
- P3 only: CORE_ENGINE_MAP.md section 13 (Honest edges), item 5 (live external tempo) and item 6 (f32 seams in the beat plumbing) for what D20 and D21 settle; ABLETON_TRANSPORT_SYNC_DESIGN.md wherever it names `NudgeBeats`; close BUG-0u1al (Link-initiated Play never caches the Link beat offset), its audit-pilot duplicate and BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point).
- Run `scripts/gen_docs_index.py`.

Total estimate: P1 and P2 landed. P3 is about 650 lines changed plus 550 of tests and source models, down from the first revision's 900 plus 900: snaps, the adaptive estimator, the visible latency field and the separate extraction commit are gone. P4 is about 150 plus 100.

## 7. What may look or feel different on stage

- **Repeatable under MANIFOLD's own clock after P1+P2.** Automation, LFOs, clip cuts and launches land on the same frame each time. Video and water are not yet covered (D16).
- **On-frame cuts move one frame earlier where they were late.** At 150 BPM on a 60 fps project, about 40% of on-beat cuts showed one frame late. They now land on their frame, in live and in export.
- **Play and resume don't repeat a frame.** A seek while playing shows the frame you seek to, then moves on.
- **Seeks land up to one frame after the click.** Invisible at normal zoom.
- **Under external sync, P1+P2 behave as today,** rounded to the nearest frame. Still stair-stepped under MIDI clock until P3.
- **After P3, the beat follows Ableton smoothly.** Under Link and MIDI clock it stays within about a millisecond of Ableton's beat. No held or skipped frames, no MIDI clock stair-steps. Under Link a tempo change or ramp lands on the frame it happens.
- **Corrections are invisible.** Drift is removed by speeding up or slowing down by at most 0.5% for a few seconds, ten times below what the eye notices. A real jump in Ableton (a loop wrap or relocation) is a seek, as today, and only a seek ever moves the beat backward.
- **A sudden big tempo change without Link eases in.** Under MIDI clock alone or AbletonOSC, MANIFOLD learns of the change a few ticks or one tempo report late. For about a second the beat then runs up to half the change away from the new tempo while it catches up: after 174 → 128 it briefly runs about 13% slow. It never jumps back, so a clip that starts on the downbeat starts once. With Link on, as on Peter's rig, Link's tempo arrives at once and none of this shows (Q5, fixed).
- **Output latency sets itself.** MANIFOLD measures how long its frames take to reach the screen and reads each source for that moment. Today's code shows the source up to a frame early by accident; after P3 the measured delay replaces it. A projector or LED processor that hides its own delay is the one thing it cannot see; the rig log prints the numbers, and a hidden preference covers that case.
- **MIDI clock and AbletonOSC line up with Link by themselves.** While Link runs beside them, MANIFOLD learns each source's offset from Link and keeps it for sets without Link.
- **If MIDI clock dies mid-set and AbletonOSC takes over, the beat carries on without a jump,** now following Live's tempo.
- **Video clips no longer jump when Ableton changes tempo.** Their start time is fixed when they start.
- **Link starts on the bar.** A Link-initiated play may move MANIFOLD's start by up to half a bar so its bar lines up with Ableton's. Link steers again after every Stop.
- **When Link peers vanish, nothing resets.** Time and the beat carry on; feedback, trails, free LFOs and video continue; the project's own tempo takes over from that beat. The last Link tempo no longer rewrites the project's tempo.
- **Timecode from LiveMTC holds within about 20 ms,** limited by its frame labels. It is the retired M4L path.
- **After a hitch, two content frames go out close together.** Presentation keeps three drawables, so this should not show. The P2 gate measures it.
- **Long stalls jump**, as today. Water slows down rather than jumping, as today, and is not repeatable under load.
- **Exports started while Ableton drives no longer freeze.** They render the arrangement on the project's tempo map.
- **A 120 Hz display, MIDI-in latency and paused visuals are unchanged.**

## 8. Decided — do not reopen

1. Integer frame index; time = index × (1 / fps as f64) from song time 0, live and export alike.
2. The content timer uses anchored grid deadlines counted by instant comparison; the UI timer is unchanged.
3. Stalls skip and stay in time; no cap.
4. Seeks snap forward (`ceil` with a 2e-7 tolerance, wider than D14's sample offset). Only a seek while playing holds. Play and resume advance normally. External seeks go to the nearest frame without a hold.
5. One tick dt (`frames × step`) for every render consumer, in every transport state.
6. External sources steer the beat by a small, slow, capped tempo trim on the follow segment, never by holding, skipping or jumping frames. The index always advances by `frames`. Two outcomes only: trim or seek; no snaps. Peter, 2026-10-14.
7. Sources are read at the frame's screen instant from timestamped arrivals, projected per source.
8. While Ableton drives, the beat follows Ableton's tempo through the follow segment; time stays on the grid; the map is the truth under Internal. Peter, 2026-10-13, extended to all four sources 2026-10-14.
9. On a Link-initiated play the offset rounds to Link's quantum. Peter, 2026-10-13.
10. Export shares the clock, set before the first seek; live equals export, not bit-identity with old export.
11. `project.settings.frame_rate` is the only fps; the engine and the timer follow it.
12. A frame's beat is taken at `time + 1e-7 × step` for comparisons; actions on the frame are recorded at its exact beat.
13. Physics and liquid stay outside the determinism claim.
14. P1+P2 land first with a named external-sync stopgap; P3 after Peter's rig test; P4 separate.
15. Export runs on the song-time-0 grid: frame k is the first export-grid frame at or after the start, plus k. Peter, Q2.
16. Output latency and source offsets are measured, not set: the present delay from `presentedTime`, the display's delay when it reports one, per-source offsets learned against Link; a hidden extra-delay preference, default 0, is the only manual part. Peter, 2026-10-14.
17. A sudden tempo change is fixed by the catch-up window: the trim may exceed 0.5% only inside a confirmed tempo change. Peter's ruling on Q5, 2026-10-14.
18. Hand-back to Internal keeps time and the beat, with a beat offset until the next Stop, seek, Play or tempo edit. 2026-10-14.

## 9. Deferred

| Item | Revive when |
|---|---|
| `frame_count` seeds in effects are tick counts since launch; generators always get 0 (`generator_renderer.rs:922`) | A particle look must repeat exactly across runs |
| Physics and liquid determinism under load (`clock.rs:125-130`, `:380`) | Peter needs the water to repeat across performances |
| Bit-equal integrator state across different drop patterns | A dt-integrating look visibly differs between two plays |
| `SetFrameRate` writes the setting outside `EditingService` (`content_commands.rs:1653-1657`) | The next settings-mutation audit |
| MIDI launch quantise inert (CORE_ENGINE_MAP.md section 13 (Honest edges), item 2) | Its own bead |
| Audio and video share mach time in Internal mode; the audio device clock may drift ±50 ppm (about 0.36 s per 2 hours) | Audio voices reseek mid-set (200 ms threshold) |
| A MANIFOLD-owned Max for Live sender of unquantised song seconds, replacing LiveMTC's frame labels (D7) | Peter brings the M4L path back into use; his 2026-07-07 ruling retired it. It would take timecode from about 20 ms to about 1 ms |
| D7's noise models and the AbletonOSC listener lag are assumptions until C0's rig log measures them | The log shows a source outside its model: retune that row from the log before C3 |
| Video playback rate follows `base_bpm`, not the trimmed rate (at most 0.5% apart outside a catch-up window) | P4 makes video sample transport time (D12); revisit only if P4 slips |
| Present delay measured on the editor window when no output window is open (D19 uses 17 ms then) | Peter performs from the editor window alone |
| A display that reports no delay of its own (D19) | A rig display hides a delay the extra-delay preference can't cover well, or Apple adds a public API |

## 10. Open questions for Peter

None. Q1 is D8, Q2 is D17, Q3 is D15. Q4 (default output latency) is closed by measurement (D19, Peter, 2026-10-14). Q5 (MIDI clock without Link on a sudden big tempo drop) is ruled "fix" and fixed by the catch-up window (D6, D20).
