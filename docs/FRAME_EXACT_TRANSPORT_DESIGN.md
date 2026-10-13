# Frame-Exact Transport — one frame grid for live play and export

**Status:** PROPOSED · 2026-10-13 · Opus lane, slot-3. Phase 1 impact map and plan, no code. Owed: adversarial review, Peter's answers in section 10, then P1–P4. Bead: BUG-gqhep (live-transport-nondeterministic-frame-clock).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

<!-- index: Frame-exact transport clock (BUG-gqhep): impact map of every transport time/beat reader and writer, dt consumer, external sync source, export and harness; the integer frame-index clock shared with export, the content-tick grid, external correction rules, phases P1–P4 and the proof plan. -->

Live play today moves the transport by measured wall time. Each frame samples whatever beat the jittered sum reached, so two plays of the same arrangement give different values on the same frame. Export does not have this problem: it computes time from a frame number. This design gives live play the same kind of clock. Time becomes a function of an integer frame index. Wall time only decides how many frames to step.

Peter's decision, 2026-10-13, quoted from the bead: "One frame-exact transport clock shared with export. Frame grid anchored at song time 0: frame_index N ↔ time N/project_fps; all consumers sample time(frame_index) computed absolutely, never accumulated; real dt only decides how many frames to advance (normally 1). Dropped frame → skip the index (stay in time). Link/MIDI clock/timecode corrections snap to whole frames (nudge the grid, never off-grid time); a correction shows as one held/skipped frame only when drift exceeds half a frame. Seek re-anchors to the grid; loops wrap the index; grid uses project fps, not display rate. Out of scope: audio-triggered clips (depend on live input)."

**For the instrument:** after this ships, an LFO, an automation lane or a clip cut lands on the same frame every time the same section plays. Under MIDI clock the picture also stops stair-stepping at the clock's 24-per-beat resolution (audit item 1.6).

Companions:
- [CORE_ENGINE_MAP.md](CORE_ENGINE_MAP.md) — the current transport map; its section 4 (Time model & tempo) and section 7 (External sync) change when this lands.
- [VSYNC_AND_FRAME_PACING.md](VSYNC_AND_FRAME_PACING.md) — the content-thread timer this design re-anchors.
- [LIVE_SIM_CLOCK_DESIGN.md](LIVE_SIM_CLOCK_DESIGN.md) — the physics clock reads transport seconds; it keeps its own cap and discard rules.
- [ENCODE_REPLAY_DESIGN.md](ENCODE_REPLAY_DESIGN.md) — GPU dispatch caching; it never reads time (audit item 1.7).

## 1. Audit — what exists (verified 2026-10-13 on 09a44cb4c)

Extend, don't redesign. Every anchor below was read on this base. Re-run the commands in each phase brief before trusting a line number.

### 1.1 How live time moves today

Each bead finding, checked against the code:

| Finding | Where | Verified state |
|---|---|---|
| The content timer returns wall dt | `crates/manifold-app/src/frame_timer.rs:360-386` (`consume_tick_at`) | True. `frame_clocked` returns the target dt instead; only harnesses set it (`frame_timer.rs:413-416`, feature `perf-soak` or test). |
| The deadline is not phase-anchored | `frame_timer.rs:161-170`, `:367` | New finding. The next deadline is `last_tick_time + period`, and `last_tick_time` is reset to "now" at each consume. Every bit of lateness is kept forever, so the tick rate runs slightly under project fps. |
| The content thread passes wall dt to the engine | `crates/manifold-app/src/content_thread.rs:576-578`, `:646-653` | True. `dt_seconds` and `pre_render_dt` both carry wall dt; `realtime_now` is wall seconds since start. |
| The engine accumulates | `crates/manifold-playback/src/engine.rs:787-792` (`advance_time`), called at `:1010` | True: `current_time_double += dt × playback_speed`, then `update_beat_from_time` (`:2133-2141`). |
| Playback speed | `engine.rs:733-735` | `set_playback_speed` has no caller (`rg 'set_playback_speed\(' crates` finds only the definition). Speed is always 1.0. |
| Automation samples the beat with no grid | `engine.rs:1054-1067`; `crates/manifold-playback/src/automation.rs:360` | True: `lane.value_at(current_beat)`. |
| External nudges | `engine.rs:796` | Correction: the inherent `nudge_time` is called only by a unit test (`engine.rs:3725`). Production nudges reach `SyncArbiterTarget::nudge_time` (`engine.rs:3326`), which calls `set_time`. |
| Export is frame-exact | `engine.rs:1001-1008` | True: `set_time(origin + frame_count × export_fixed_dt)`. Origin is the export start, not song time 0 (`crates/manifold-app/src/content_export.rs:714-717`, `:796`). |

The Play press has no phase anchor. `ContentCommand::Play` (`crates/manifold-app/src/content_commands.rs:880-913`) flips the state. The next tick then adds whatever wall time passed since the previous tick, which depends on where between two ticks Play was pressed.

### 1.2 Readers and writers of transport time and beat

Classes: **GRID** = must sample the frame grid. **WALL** = must keep real wall time. **DERIVED** = reads a value already computed from the transport, so it changes for free. **EXT** = an external source moving the transport; becomes a correction.

Writers (each is replaced or rerouted by this design):

| Writer | Where | Class | After |
|---|---|---|---|
| Live advance | `engine.rs:1010` → `:787` | GRID | Deleted. The clock steps its index. |
| Export advance | `engine.rs:1001-1008` | GRID | The same clock with an export origin (D9). |
| `set_time` | `engine.rs:692-697`; callers `content_commands.rs:909` (Play aligns to MIDI clock), `engine.rs:3326` (trait nudge), tests | GRID | Renamed `locate` (D4); trait nudge becomes a correction (P3). |
| `seek_to` | `engine.rs:803-853`; callers `content_commands.rs:949`, `:963`; `crates/manifold-playback/src/transport_controller.rs:184`, `:196`; `content_export.rs:427`, `:497`, `:717`, `:789`; arbiter `engine.rs:3330` | GRID | Snaps to the grid and arms the anchor (D4). |
| `set_beat` + `sync_time_from_beat` | `engine.rs:699-717`; callers `content_thread.rs:1938-1940` (Link), `:1959-1961` (MIDI clock), `content_commands.rs:907` (Play) | EXT | Become correction targets (P3). Deleted. |
| `set_beat` after a tempo-map change | `content_thread.rs:2110-2111` | DERIVED | `rederive_beat()`: beat from clock time. |
| Tempo edit reconcile | `engine.rs:721-731`; callers `content_commands.rs:2218`, `transport_controller.rs:279` | GRID | Keep the beat, remap to the first frame at or after (D11). |
| Stop / initialize | `engine.rs:675-677`, `:579-581` | GRID | Index 0. |
| `set_external_time_sync` | `crates/manifold-playback/src/midi_clock_sync.rs:591`, `crates/manifold-playback/src/osc_sync.rs:373`, `transport_controller.rs:103` | EXT | Deleted in P3. External sources never stop the grid (D6). |

Readers inside the engine:

| Reader | Where | Class |
|---|---|---|
| `sync_clips_to_time` membership, backward-move guard | `engine.rs:1625-1740` | DERIVED (the backward guard at `:1633-1642` matters for D6) |
| Timeline query | `engine.rs:1301-1367` | DERIVED |
| Automation (play / inspect / previews) | `engine.rs:1054-1078`, `:1178-1201` | DERIVED |
| Modulation: drivers, envelopes, trigger stamps | `engine.rs:1095-1108`, `:1232-1248`, `:933-941` | DERIVED (`dt` there is the tick dt, audit item 1.3) |
| Session launch, stop, quantize, seek retarget | `engine.rs:816`, `:1854-1980` | DERIVED |
| One-shot expiry | `engine.rs:2296-2301` | DERIVED |
| Video rate, expected source time, drift cadence | `engine.rs:2494-2530`, `:2746-2866`, `:1125-1130` | DERIVED |
| `pre_render` for every renderer | `engine.rs:2961-2968` | DERIVED time; dt per audit item 1.3 |
| Project BPM display write | `engine.rs:2362-2384` | DERIVED |
| `SyncTarget::current_time`, `LiveClipHost::current_beat` | `engine.rs:3175-3178`, `:3284` | DERIVED |

Readers outside the engine:

| Reader | Where | Class |
|---|---|---|
| Generator and compositor frame time/beat | `crates/manifold-app/src/content_pipeline.rs:2235-2236` → `:2524-2527`, `:2801-2806`; clip atlas `:3189` | DERIVED |
| Node-graph frame clock | `crates/manifold-node-engine/src/runtime/core.rs:1681-1688`, `:1942-1948` → `FrameTime` | DERIVED |
| Primitives sampling absolute time or beat | `lfo.rs:170`, `:180`; `beat_ramp.rs:86`; `trigger_ease_to.rs:201`; `terminal_stream.rs:162`; `flow_field_noise.rs:143`; `scanline_jitter_field.rs:197`; `track_regions.rs:478` (nodes-image); `loop_camera.rs:294`; `glitch_jitter.rs:140`; `noise_displace.rs:139`; `ripple_mesh.rs:158`; `ocean_spectrum.rs:124`; `render_lines.rs:533`; `gltf_animation_source.rs:339`, `:409`; `gltf_skeleton_pose.rs:552`, `:632`; `gltf_morph_weights.rs:275`, `:346` (nodes-scene) | DERIVED |
| Physics and liquid clocks | `crates/manifold-water-rigid/src/primitives/physics_world.rs:467`; `crates/manifold-water-gpu-mpm/src/primitives/matter_domain.rs:590-953`; `crates/manifold-water-surface/src/primitives/liquid_frame.rs:209` → `crates/manifold-physics/src/clock.rs:278` | DERIVED (audit item 1.3) |
| MIDI note stamps, one-shots, recording | `crates/manifold-playback/src/live_clip_manager.rs:425-576`, `:959`, `:1011` | DERIVED |
| Clip-launch callbacks | `crates/manifold-playback/src/clip_launcher.rs:181`, `:268`, `:405`, `:516` | DERIVED (report only) |
| Audio-layer voices | `crates/manifold-playback/src/audio_layer_playback.rs:324` | DERIVED beat; the voice keeps the audio clock (WALL) |
| Outbound Ableton / M4L beat | `content_thread.rs:670-675`, `:691-697` | DERIVED beat; retransmit windows are WALL |
| Tempo recorder stamps | `content_thread.rs:2056-2081` | DERIVED |
| Link offset cache | `content_thread.rs:1973-1987` | EXT (audit item 1.6) |
| External position compares | `midi_clock_sync.rs:777-797`, `osc_sync.rs:410-428` | EXT |
| Seek echo to Ableton | `content_commands.rs:952`, `:965` | DERIVED |
| `FireParameter` frame stamp | `content_commands.rs:1147-1152` | DERIVED |
| Audio analysis epoch | `crates/manifold-app/src/audio_mod_runtime.rs:337-345` (`transport_epoch`) | DERIVED; corrections must not bump the epoch (D6) |
| UI playhead, transport bar, perform HUD, autosave | `crates/manifold-app/src/content_state.rs:119-121` → `ui_bridge/state_sync.rs:435-436`, `ui_bridge/projection/transport.rs:123`, `perform_mode/render.rs:97`, `autosave.rs:268` | DERIVED. No interpolation: `rg 'extrapolat|dead.?reckon' crates/manifold-app/src` finds nothing on the playhead path. |
| Harness readers | `perf_soak.rs:558`, `:983`; `gap_start_probe.rs:94`, `:105`; `scene_modifier_journey.rs:483`; `scene_modifier_journey/periodic.rs:231`; `scene_modifier_journey/angular.rs:210`; `corridor_acceptance.rs:388`; `gpu_flip_export_demo.rs:197`, `:202` | DERIVED |

LED output reads no time (`rg '\bbeat\b|\btime\b|\bdt\b|Instant' crates/manifold-led/src` has no hits outside tests).

Re-derive: `rg -n '\.(current_time_double|current_time|current_beat|current_beat_f64|time_to_timeline_beat|beat_to_timeline_time|advance_time|nudge_time|set_time|set_beat|sync_time_from_beat|seek_to|set_export_origin|transport_epoch)\(' crates -g '*.rs'`. On this base it hits 24 files outside `engine.rs`.

### 1.3 Where dt flows past the transport

Today one wall dt feeds everything below. After the change the **tick dt** is `frames × step`: the content-tick frames since the last tick times one project frame (D5). It equals the transport step while playing, except on the anchor tick.

| Consumer | Where | Uses today | After | If frame-quantised instead of wall |
|---|---|---|---|---|
| Generators (`PresetContext.dt`) | `content_pipeline.rs:2527` → `crates/manifold-compositor/src/generator_renderer.rs:911-925` | wall dt | tick dt | Nothing breaks: average equal, jitter gone. |
| Compositor / effect chains (`CompositorFrame.dt`) | `content_pipeline.rs:2804` | wall dt | tick dt | Same. |
| dt-integrating primitives | `smoothing.rs:142`, `one_euro_filter.rs:126`, `compressor_envelope.rs:172` (nodes-image); `nested_cubes_geometry.rs:226`, `render_lines.rs:534` (nodes-scene); particle `frame_delta = delta × 60` at `crates/manifold-node-engine/src/scene/boundary_nodes.rs:402` | wall dt, also while paused | tick dt, also while paused | Paused graphs keep animating, as today. Frame drops still change their history (they integrate). |
| Temporal reset detector | `crates/manifold-node-engine/src/exec/temporal_reset.rs:63-76`; `render_mesh_diagram.rs:322-336` | resets when Δseconds and delta disagree by over 1.5× | unchanged rule | Safe: while playing Δseconds = tick dt exactly. The anchor tick has Δseconds 0 and delta one frame, which is inside the rule (same as paused today). |
| Modulation audio followers | `crates/manifold-playback/src/modulation.rs:378`, `:930-935` | wall dt | tick dt | Live audio input; not part of determinism. |
| Audio clip triggers | `engine.rs:1024`, `:2272-2290` | wall dt | tick dt | Live input; out of scope per Peter. |
| Video decode pacing | `crates/manifold-media/src/video_renderer.rs:772-838` | `time_accumulator += dt × rate` | P4: target frame from transport time | Today video is an integrator, not a sample. It breaks Peter's "never accumulated" rule. Drift correction re-seeks only every 2 s when 0.1 s off (`engine.rs:1125-1130`, `:2746`). |
| Physics / liquid sim | `crates/manifold-physics/src/clock.rs:278-400` via transport seconds | transport seconds | unchanged code | Becomes deterministic for free: the Sim Rate grid anchors at the first observed transport time (`clock.rs:363-372`) and restarts on any backward step. Its live cap (two intervals, then discard) still owns overload. |
| Sim late-frame rule | `content_thread.rs:790-803` (render work), `content_thread.rs:425-431` (fence wait) | wall ms | unchanged (WALL) | Must stay wall: it measures load. |
| MIDI NoteOff 5 ms guard | `live_clip_manager.rs:40`, `:761` | wall `realtime_now` | unchanged (WALL) | A human-input debounce; quantising it would reject real notes. |
| Compositor dirty deadlines, recently-started gate, pending pauses, prewarm cadence | `engine.rs:850`, `:1484-1530`, `:2921-2942`, `:3026-3050` | wall `realtime_now` | unchanged (WALL) | Decode-latency guards; wall by nature. |
| Sync controllers, arbiter cooldowns, Ableton retransmit | `content_thread.rs:1694-1913`; `sync.rs`; `crates/manifold-playback/src/ableton_bridge.rs:2404-2429`; `crates/manifold-playback/src/osc_sender.rs:113-137` | wall `time_since_start` | unchanged (WALL) | Round-trip timing to other machines. |
| Percussion status timer | `content_thread.rs:709-717` → `percussion_orchestrator.rs:217-232` | wall | unchanged (WALL) | UI status only. |
| Audio-layer voices | `audio_layer_playback.rs:51-55` | own audio clock; reseek over 200 ms (60 ms paused) | unchanged | A frame of difference never reaches the 200 ms threshold. |
| UI thread animation | `crates/manifold-app/src/app_render.rs:220-222` (its own `FrameTimer`, `app.rs:702`) | wall | unchanged (WALL) | The UI timer is a separate instance; D2 must not change it (`app.rs:3135-3143` calls `should_tick`). |
| `frame_count` handed to graphs | effects: content tick count (`content_pipeline.rs:2806`); generators: always 0 (`generator_renderer.rs:922`) | tick counter | unchanged | Out of scope; listed in Deferred. |

### 1.4 Tempo

Time is the integration axis and beats are derived, in live play and in export alike. `advance_time` and the export branch both end in `update_beat_from_time` → `TempoMapConverter::seconds_to_beat_f64` (`engine.rs:2133-2141`, `crates/manifold-core/src/tempo.rs:404-408`). The map is piecewise constant BPM, integrated exactly (`tempo.rs:288-329`). So `beat(N) = map(time(N))` is supported as is, for any tempo map.

Every loaded project has a beat-0 tempo point (`crates/manifold-core/src/project/load_migration.rs:75`). So `settings.bpm` is never the conversion tempo for a loaded project. This matters for external sync (audit item 1.6).

Two writers change the map or BPM under a playing transport:
- User tempo edits keep the beat and move time (`engine.rs:721-731`). That puts time off the grid. D11 snaps it.
- `apply_resolved_tempo` writes a live external tempo into the beat-0 point under Internal authority (`content_thread.rs:2083-2103`), then keeps time and moves the beat (`:2110-2111`). Live tempo is only cleared when there is no project (`content_thread.rs:2033-2036`). So when Link peers vanish mid-show, the stale Link tempo stays and this branch can fire. The beat then jumps. Pre-existing; listed in Deferred.

### 1.5 Project fps, display rate, content tick rate

- The content timer runs at `settings.frame_rate` (`f32`, `crates/manifold-core/src/settings.rs:26`). It is set on load (`content_commands.rs:1491-1493`), on `SetFrameRate` (`content_commands.rs:1653-1657`, which also writes the setting directly, outside `EditingService`) and after undo/redo when fps changed (`content_commands.rs:1357-1359`, `:1435-1437`).
- The display rate never drives the content thread. Metal's `displaySyncEnabled` is the only vsync gate (VSYNC_AND_FRAME_PACING.md, "Output Presentation"). At 120 Hz each 60 fps content frame is already shown twice today. Nothing here changes that.
- Slower content thread: today a long tick gives a long dt. After the change it gives `frames > 1`: the index skips and stays in time. Same picture, now on grid.
- Non-integer fps: f32 29.97 is 1.02e-6 slower than 30000/1001. Over a 2-hour set that is 7.4 ms, under half a frame (16.7 ms). Computed by script, not estimated. The live grid and the content timer use the same f32 rate, so they agree with each other exactly. Export already stamps the container with the exact rational (`crates/manifold-media/src/frame_rate.rs:40-61`, BUG-129 (export NTSC timebase drift)). No rational type is needed for the transport (D1).
- Float error: `index as f64 × step` differs from the exact `index / fps` by at most 9e-13 s at frame 432,000 (2 hours at 60 fps), for 60, 24 and f32 29.97. For contrast, 432,000 additions of 1/60 drift 2e-8 s. Neither matters beside wall jitter; the integer index matters because it makes every run take the same path.
- Mid-session fps change: the timer re-targets (`frame_timer.rs:429-437`). D10 re-anchors the clock.

### 1.6 External sync today

Authority is chosen every frame: MIDI clock receiving, then OSC receiving, then Link with peers, then Internal (`content_thread.rs:1706-1743`).

| Source | How it moves the transport today | Finding |
|---|---|---|
| MIDI clock | While receiving and playing, `external_time_sync = true` stops the engine advancing (`midi_clock_sync.rs:585-596`, `engine.rs:1000`). Each frame sets the beat to the last clock tick (`content_thread.rs:1943-1962`). The beat is `(sixteenths + tick/6)/4` (`midi_clock_sync.rs:368-370`): 24 steps per beat with no interpolation. The receiver stores no tick timestamps (`midi_clock_sync.rs:19-58`). | **The picture stair-steps today.** One tick is 20.8 ms at 120 BPM, longer than a 16.7 ms frame. 48 ticks per second over 60 frames means about 12 repeated beat values every second. At 174 BPM the steps are uneven instead. Nudge under 2 s, hard seek otherwise (`midi_clock_sync.rs:777-797`). |
| Link | Never sets `external_time_sync`. Each frame, when MANIFOLD does not own playback and the offset is valid, the beat becomes `link_beat − offset` and time is re-derived (`content_thread.rs:1928-1941`). The engine then adds a wall dt on top. | The offset is cached only by MANIFOLD's own Play/Seek (`content_commands.rs:912`, `:958`, `:970`). Stop and load poison it with NaN (`:936`, `:1470`). A Link-initiated Play (`crates/manifold-playback/src/link_sync.rs:150-152`) never caches it. After any Stop, Link never steers position again until MANIFOLD seeks. There is no quantum phase alignment at Play: the offset is whatever it was. |
| OSC timecode (M4L) | Receiving sets `external_time_sync` (`osc_sync.rs:368-373`). Each message nudges under 0.5 s, seeks otherwise (`osc_sync.rs:405-428`). | Timecode arrives at 25–30 Hz, so a 60 fps transport moves only when a message lands: about every other frame. |
| AbletonOSC relay | Play / Pause / SeekBeats / NudgeBeats at listener cadence, CLK-gated (`content_thread.rs:1858-1912`). | Same nudge path as above. |

**Tempo mismatch.** Under any external source, the target time is `map(external beat)` through the project tempo map. When Ableton's tempo differs from the map, map time runs faster or slower than wall time. Today that is invisible: the beat is copied each frame. On a fixed time grid it shows up as steady drift. At 0.1 BPM off (128 vs 128.1) that is one corrected frame every 21 s. At 120 vs 128 it is four per second. Computed by script. This is open question Q1.

### 1.7 Export and encode replay

- Export: seek to start, `play()`, optional warm-up ticks with an accumulating clock, re-seek, then `set_export_origin(start)` and frames `0..total` with `export_fixed_dt = 1.0 / fps as f64` (`content_export.rs:566-574`, `:714-796`, `:812-852`). Frame 0 renders the start with dt 0 (`content_export.rs:960-973`). Export fps comes from `ExportConfig.fps: f32` (`crates/manifold-media/src/export_config.rs:13`) and may differ from project fps.
- The origin is the export start. When that start is off the song-time-0 grid, live and export sample different times for the same song moment. Example: at 128 BPM a bar is 112.5 frames at 60 fps, so every odd bar starts off grid. This is open question Q2.
- Encode replay never reads time. `rg -n -i '\bbeats?\b|transport|current_time|frame_time|Seconds' crates/manifold-gpu/src/replay.rs crates/manifold-gpu/src/metal/replay.rs crates/manifold-gpu/src/vulkan/replay.rs` has no hits, and manifold-gpu depends on neither core nor playback. It is outside this design.

### 1.8 Harnesses and tests that lean on today's behaviour

- `frame_clocked`: 24 call sites set it (`rg -c 'set_frame_clocked\(' crates`): rt-capture (BUG-jbxt (rt-capture frame clock)), corridor acceptance, scene-modifier journeys, mute-visibility and gap-start probes, blob and scene-modifier performance, `frame_time.rs:843`, `rt_dynamic_held_out.rs:318`, and 3 timer tests. They need exactly one project frame per tick, which D2 keeps.
- `manifold frame-time` (`crates/manifold-app/src/frame_time.rs:1-33`) measures production pacing headless. It is the oracle for the timer change.
- `TickContext` struct literals: 33 sites (`rg -n 'TickContext \{' crates -g '*.rs'`): engine tests 3, `tests/engine_tick.rs` 15, `tests/param_step_clip_edge.rs` 3, `tests/sync_engine_integration.rs` 2, `tests/session_mode.rs` 1, `tests/trigger_source_switch.rs` 1, `content_thread.rs` 1, `content_export.rs` 2, `content_pipeline/trigger_lane_force_tests.rs` 2, `bug219_verify.rs` 1, `import_responsiveness_verify.rs` 1, `ui_snapshot/script.rs` 1. Plus `TickContext::default()` in engine unit tests (zero dt means no advance, same after).
- Tests that step by non-frame amounts: `tests/param_step_clip_edge.rs` uses 0.25, 0.295, 0.005, 0.5, 0.1, 0.03, 0.01 and 1000.0 s (`:96-107`, `:200-219`). They prove clip-control coverage is step-size independent. That still matters for `frames > 1`. The mapping is in the P2 seam brief.
- Tests that assume accumulation within a tolerance: `tests/engine_tick.rs:422-458` (1 s of 1/60 steps ≈ 1.0 s). They pass unchanged on the grid.
- Timer tests: `frame_timer.rs:518-531` (load excluded from dt), `:550-569` (wall telemetry with frame clock), `:636-647` (frame clock fixed dt).
- `set_state(PlaybackState::Playing)` bypasses `play()`; all 37 callers are tests (`rg -c '\.set_state\(PlaybackState' crates`).

### 1.9 Dead and stale things found on the way

- Inherent `PlaybackEngine::nudge_time` (`engine.rs:796`): test-only caller. Deleted in P2.
- `on_time_changed`, `beat_snapped_beat_resolver`, `absolute_tick_resolver`, `replenish_warm_cache`: never installed (only the `None` initialisers at `engine.rs:369-375`). `on_time_changed` fires inside the advance block (`engine.rs:1014-1017`) and is deleted with it in P2. The others are outside this scope.
- `playback_speed` (`engine.rs:119`, `:422`, `:733`): no writer. Deleted in P2; a frame grid cannot step by 0.37 frames.
- CORE_ENGINE_MAP.md section 13 (Honest edges), item 2, names `activate_due_pending_launches_at_tick`. That function no longer exists (`rg` finds nothing). Fixed in the landing sweep.
- CORE_ENGINE_MAP.md section 3 (The frame, end to end) says the inherent `nudge_time` is the nudge path. Fixed in the landing sweep.

## 2. Decisions

**D1 — One clock type, integer index, absolute time.** `TransportClock` holds an `i64` frame index, an origin and a step. Time is always `origin + index as f64 × step`, with `step = 1.0 / fps as f64` from the `f32` fps. Live play uses origin 0. Nothing ever adds to time. This expression is exactly the export expression today, so export stays bit-identical (D9).
Rejected: an exact rational `(num, den)` type. Its benefit is 7.4 ms over 2 hours at 29.97, under half a frame, and it would change export bits. Rejected: keeping a time accumulator with a grid snap at read time. It reintroduces the path dependence this bead removes.

**D2 — The content tick runs on an anchored grid.** The content thread's timer puts deadline n at `anchor + n × step`. It reports `frames`: the number of grid deadlines passed since the last tick. Normally that is 1. A missed deadline gives 2 or more. The anchor resets on load and on an fps change. With `frame_clocked`, `frames` is always 1. The UI thread's timer keeps today's free-running deadline.
Why anchored: with today's free-running deadline every late microsecond is kept. Counting grid frames against wall time would then skip a transport frame about once a minute at 5 µs of lateness per tick, and every 2.8 s at 100 µs. Computed. With anchored deadlines, sub-frame jitter can never change `frames`.
Rejected: `round(elapsed × fps)` with hysteresis. Anchored deadlines make it unnecessary, and rounding turns a 0.6-frame late tick into a skip followed by a hold.

**D3 — Stalls skip and stay in time.** After a stall of any length, `frames` is the number of deadlines passed, and the transport skips that many. There is no cap, as today: nothing clamps dt now (`rg 'dt\.min\(|dt\.clamp\(' crates` finds only solver-internal limits). The physics clock keeps its own rule: at most two Sim Rate intervals per frame, discard the rest (LIVE_SIM_CLOCK_DESIGN.md section 7 (Decided — do not reopen)). The two rules fit together: the transport keeps time, the water goes slow-motion. The 5 s GPU surface timeout (`content_thread.rs:485`, `:498-502`) is the longest stall the content thread itself can create.

**D4 — Re-anchors snap forward and render the anchor first.** Play, resume, seek, `locate` and an external hard seek move the index to the first grid frame at or after the target: `ceil(t / step − 1e-7)`. The next playing tick renders that frame and advances 0 frames. After that it advances by `frames`. This mirrors export frame 0. A one-in-ten-million tolerance absorbs float error, as `Driver::evaluate_frame_aligned` already does (`crates/manifold-core/src/effects/driver.rs:189-200`). A scripted round trip at nine rates over frames 0–5,000 and 431,000–433,000 had zero mismatches.
Why "at or after": a clip spans `[start, end)`, as in export. The first frame that shows a clip starting at `s` is `ceil(s / step)`. Seeking to a clip's start must show the clip.
Rejected: nearest rounding, because a seek to a clip start could land just before it and show black. Rejected: advancing on the Play tick, because then the frame the press landed on depends on the press and the first frame never matches export frame 0.

**D5 — One tick dt for everything that renders.** `tick_dt = frames × step`, in every transport state. It feeds generators, effects, the compositor, modulation's audio followers, audio triggers and video `pre_render`. The transport advances by the same `frames` while playing (by 0 on the anchor tick). Export frame 0 has `frames = 0`, so `tick_dt = 0` as today.
Rejected: a zero dt while paused. Paused graphs (smoothing, particles, feedback) would freeze, and a soundcheck with the transport stopped would look dead.

**D6 — External sources correct the grid; they never drive it.** `external_time_sync` is deleted. The grid always advances by `frames`. Each external source feeds a target time for the frame about to render. The clock filters the drift (target minus grid time) and acts:
- |drift| ≤ ½ frame: nothing.
- ½ frame < |drift| ≤ 2 frames: one frame per tick. A positive drift adds one frame (a skip). A negative drift removes one frame from this tick's advance, at most down to 0 (a hold). The index never moves backward.
- 2 frames < |drift| < the source's seek threshold: a jump to the nearest grid frame this tick. Same effect as today's nudge, but on the grid.
- At or above the seek threshold, or a transport jump: the existing seek path.

Corrections and jumps do not bump `transport_epoch`, and they do not run seek side effects. A backward jump still trips the backward-move guard in `sync_clips_to_time` (`engine.rs:1633-1642`), which today's backward nudges already do.
Why "never backward on a slew": a one-frame backward step would reset trigger delivery every time it happens. Why jump above 2 frames: slewing 1.9 s of drift one frame at a time would play double speed or freeze for 1.9 s.

**D7 — How each source is sampled.**
- Link: sampled exactly at this frame's deadline. Read `link.clock_micros()` and `Instant::now()` back to back to map the deadline into Link time, then `beat_at_time(deadline_micros, quantum)`. No filter.
- MIDI clock, OSC timecode and AbletonOSC nudges are coarse: one MIDI tick is longer than a frame, and timecode is quantised to its own frames. A sample is taken only on a frame where a new value arrived. It is bias-corrected by +½ frame, because the value arrived somewhere within the last frame interval. Then it goes through an exponential average with α = 0.1. For uniform one-frame quantisation noise (σ = 4.8 ms at 60 fps) the averaged σ is 1.1 ms, 7σ inside the half-frame threshold.
- Default, with a trigger: the receiver keeps no tick timestamps. If the P3 simulation (`phase_lock_ignores_clock_quantisation`) shows any false correction over 2 simulated hours at matched tempo, stop and escalate. The fix then is to timestamp ticks in the midir callback, which is a receiver change.
- Seek thresholds stay as today: MIDI clock 2.0 s, OSC 0.5 s.
- ⚠ VERIFY-AT-IMPL: that `rusty_link`'s `beat_at_time` accepts an arbitrary micros value, and that `clock_micros` shares the host clock — read `link_sync.rs:96-131` and the rusty_link `SessionState` docs.

**D8 — Time is the grid axis; beats come from the tempo map.** `beat(N) = map(time(N))` under every authority. External sources give a target time `map(external beat)` (Link, MIDI clock, AbletonOSC) or a time directly (timecode). Q1 asks Peter whether the live tempo should replace the map while an external source is in charge. The default is no.

**D9 — Export shares the clock and stays bit-identical.** Export calls `begin_export_clock(origin = start_time, fps = export_config.fps)`. Frame `k` sets index `k` exactly. Time is `origin + k as f64 × (1.0 / fps as f64)`, the same expression and the same bits as `engine.rs:1006-1008` today. The end of export restores the live rate and seeks to the restore time as today (`content_export.rs:427`, `:497`). Q2 asks whether export should anchor at song time 0 instead.

**D10 — An fps change remaps.** `set_frame_rate(fps)` keeps the current time and moves the index to the first frame at or after it under the new step, with no anchor hold. The timer re-anchors its grid in the same command.

**D11 — Tempo edits during play keep the musical position.** After a map or BPM edit, `reconcile_tempo_edit` keeps the beat and remaps to the first frame at or after `map'(beat)`, with no anchor hold. Today it skips this while an external source is in charge (`engine.rs:722`). After P3 it always runs, except under OSC timecode, where time is the master: then it keeps the index and re-derives the beat.

**D12 — Video picks its frame from transport time.** Each tick, a playing video clip's target source frame is `floor(video_time × video_fps)`, from `compute_video_time` (`engine.rs:2559`). It requests that frame when it differs from the one shown. It seeks when the gap exceeds the existing 3-frame skip rule. Decode stays asynchronous: a late decode holds the previous frame, as today. The 2 s drift correction stays as a safety net. P4 owns this.

**D13 — What stays on wall time.** The content timer's own telemetry (FPS, lateness, missed ticks), `realtime_now` and every deadline built on it (audit item 1.3), the sim late-frame rule, MIDI input guards, sync-controller windows, the UI thread, audio voices and audio analysis. None of these may read the transport frame index.

## 3. Design

### 3.1 `TransportClock` — `crates/manifold-playback/src/transport_clock.rs` (new)

Owned by `PlaybackEngine`, content thread only. Pure: no `Instant`, no allocation. It lives in playback because the engine owns it and export drives the engine. manifold-core holds data models, not runtime state. No crate boundary moves.

Precedents: `manifold_physics::clock::SimulationClock` (`crates/manifold-physics/src/clock.rs:278-400`) for absolute time from an integer count (`transport_origin + transport_end as f64 × interval`). `Driver::evaluate_frame_aligned` (`driver.rs:189-200`) for the frame-lattice tolerance.

```rust
/// Transport time as an integer frame index on a fixed step.
/// time(index) = origin + index as f64 * step, never accumulated.
#[derive(Debug, Clone)]
pub struct TransportClock {
    origin: Seconds,      // 0 live; export start during export
    step: f64,            // 1.0 / fps as f64, fps from the f32 setting
    fps: f32,
    index: i64,
    anchor_pending: bool, // next playing advance renders `index` first (D4)
    lock: PhaseLock,      // external drift filter (D6, D7); idle when no source
    last_advance: u32,    // frames the transport moved on the last tick
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction { None, Skip, Hold, Jump(i64) }

impl TransportClock {
    pub fn new(fps: f32) -> Self;
    pub fn time(&self) -> Seconds;
    pub fn index(&self) -> i64;
    pub fn step(&self) -> Seconds;
    /// First frame at or after `t`: ceil((t - origin) / step - 1e-7).
    pub fn index_at_or_after(&self, t: Seconds) -> i64;
    pub fn index_nearest(&self, t: Seconds) -> i64;
    /// Play, resume, seek, locate: snap forward and hold the next advance (D4).
    pub fn anchor_at(&mut self, t: Seconds);
    /// Tempo edit, fps change: snap forward, no hold (D10, D11).
    pub fn remap_at(&mut self, t: Seconds);
    pub fn reset(&mut self); // index 0, no hold, lock cleared (Stop, initialize)
    /// Called once per playing tick. Applies the anchor hold, then `frames`,
    /// then at most one pending correction. Returns the frames moved (>= 0).
    pub fn advance(&mut self, frames: u32) -> u32;
    pub fn set_fps(&mut self, fps: f32);                       // D10
    pub fn begin_export(&mut self, origin: Seconds, fps: f32); // D9
    pub fn end_export(&mut self, live_fps: f32);
    /// Export frame k sets the index exactly (D9).
    pub fn set_export_frame(&mut self, k: u32);
    /// P3: one external target for the frame about to render (D6, D7).
    pub fn observe_external(&mut self, target: Seconds, filter: ExternalFilter);
    pub fn clear_external(&mut self);
    pub fn last_correction(&self) -> Correction;
}

#[derive(Debug, Clone, Copy)]
pub enum ExternalFilter {
    /// Link: exact sample at the frame deadline.
    Exact,
    /// MIDI clock, timecode, AbletonOSC: +½ frame bias, average α = 0.1.
    Coarse,
}
```

`PhaseLock` is private to the module: an averaged drift in frames, the 2-frame slew limit and the per-tick decision. Seeks stay with the callers' existing thresholds (D7). Hard seeks go through `PlaybackEngine::seek_to`, never through the lock.

The tempting wrong turn: you will want to keep `current_time_double` and "snap it" after each `+= dt`. No. That is still an accumulator, and the snap direction then depends on the jitter. The index is the state; time is computed from it.

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
| fields `current_time_double`, `current_time`, `playback_speed`, `external_time_sync`, `export_origin_seconds`, `on_time_changed` | `clock: TransportClock`; `current_beat: f64` stays a cache, refreshed after every clock change |
| `advance_time(dt)`, inherent `nudge_time(t)` | deleted; `tick_playing` calls `clock.advance(ctx.frames)` |
| `set_time(t)` | `locate(t)` = `clock.anchor_at(t)` + beat refresh |
| `seek_to(t)` | unchanged contract plus `clock.anchor_at(t)` |
| `set_beat(b)` + `sync_time_from_beat()` | P2: `follow_external_beat(b)` = `clock.remap_at` to the nearest frame of `map(b)`. P3: deleted. External sources call `observe_external` |
| `set_export_origin(t)` + `export_fixed_dt` | `begin_export_clock(t, fps)`, `end_export_clock()`; while export mode is on, the tick calls `clock.set_export_frame(ctx.frame_count)` |
| `set_external_time_sync`, `external_time_sync()` (engine and `SyncArbiterTarget`) | deleted in P3 |
| — | `set_frame_rate(fps: f32)` (D10), `transport_frame() -> i64`, `tick_dt() -> Seconds` (D5), `rederive_beat()` (audit item 1.2) |
| `current_time()`, `current_time_double()` | now read `clock.time()` |

`tick_dt()` is what the content thread passes to `render_content` in place of wall dt, and what `filter_ready_clips`, modulation and audio triggers read in place of `ctx.dt_seconds` / `ctx.pre_render_dt`.

### 3.3 `FrameTimer` — `crates/manifold-app/src/frame_timer.rs`

```rust
pub struct TickFrames { pub frames: u32, pub wall_dt: f64 }

impl FrameTimer {
    pub fn new(target_fps: f64) -> Self;      // free-running, UI thread, unchanged
    pub fn new_grid(target_fps: f64) -> Self; // content thread: deadlines on anchor + n * step
    pub fn consume_frames(&mut self) -> TickFrames; // content thread
    pub fn consume_tick(&mut self) -> f64;    // UI thread, unchanged
}
```

In grid mode the deadline is `anchor + Duration::from_secs_f64(n as f64 × step)`, never `n × (rounded period)`, so the timer and transport steps agree. `frames = max(n_now − n_last, 1)`, with `n_now = floor((now − anchor) / step + 1e-9)`. Production always waits for the deadline, so a 0 can only come from a harness ticking early. `frame_clocked` gives `frames = 1` and `realtime_since_start = total_frames × step`. `missed_ticks` becomes `frames − 1`. `resume_after_load` and `set_target_fps` re-anchor at now. Wall telemetry (`last_wall_interval`, EWMA FPS) is unchanged.

Consequence, stated honestly: after a tick that is late by less than a frame, the next deadline is closer than a full period. Two content frames go out back to back. That is how every fixed-step game loop behaves. It can make the GPU surface wait and the sim's late-frame rule (LIVE_SIM_CLOCK_DESIGN.md section 8 (Resolved decisions and deferred scope)) see one extra late frame after a hitch. The P2 gate measures it.

### 3.4 Content thread

- `tick_frame` (`content_thread.rs:574-653`): `let tf = self.timer.consume_frames();` → `TickContext { frames: tf.frames, realtime_now, frame_count }`. `render_content(…, self.engine.tick_dt().0, …)`.
- `ContentThread` builds its timer with `FrameTimer::new_grid` (app `app.rs:2556`, `headless_harness.rs:92`).
- Every `self.timer.set_target_fps(x)` (`content_commands.rs:1358`, `:1436`, `:1492`, `:1656`; `rt_capture.rs:483`, `:498`) is paired with `self.engine.set_frame_rate(x as f32)`. P2 adds a test that the two can't diverge (section 4).
- `derive_external_beat` (`content_thread.rs:1922-1968`): P2 calls `follow_external_beat`. P3 replaces it with `observe_external`.
- `cache_link_beat_offset` also runs on every transition into Playing that Link authority caused (P3). That fixes the NaN gap in audit item 1.6.
- Play aligned to MIDI clock (`content_commands.rs:900-910`) becomes `engine.locate(map(clock beat))`.

### 3.5 External sources (P3)

| Source | Today | After |
|---|---|---|
| MIDI clock | `set_external_time_sync` (`midi_clock_sync.rs:591`); nudge and seek (`:777-797`); per-frame beat copy (`content_thread.rs:1951-1962`) | Transport play/pause unchanged. Position: a `Coarse` sample on frames where the tick changed, target `map(clock beat)`. Seek unchanged (2 s and SPP jumps). The per-frame beat copy is deleted. |
| Link | per-frame beat copy (`content_thread.rs:1928-1941`) | An `Exact` sample at the frame deadline, target `map(link beat − offset)`, while the existing gates hold (not MANIFOLD-owned, offset valid). |
| OSC timecode | `set_external_time_sync` (`osc_sync.rs:373`); nudge and seek (`:405-428`) | `Coarse` sample per message, target = timecode seconds. Seek unchanged (0.5 s). |
| AbletonOSC relay | `nudge_time` / `seek` actions (`content_thread.rs:1888-1909`) | NudgeBeats becomes a `Coarse` sample; SeekBeats unchanged. |
| Arbiter | `SyncArbiterTarget::nudge_time`, `set_external_time_sync` (`crates/manifold-playback/src/sync.rs:23-27`, `:181-228`) | `nudge_time(time)` becomes `observe_external(time, filter)` with the gate unchanged; `set_external_time_sync` deleted. The gate-matrix tests (`sync.rs:318-357`) move with it. |

### 3.6 Edge cases

| Case | Behaviour after |
|---|---|
| Play | Anchor at the snapped current time. The first playing frame repeats the paused frame, then steps. |
| Pause / resume | Index kept. Resume is an anchor: one repeated frame. |
| Stop | Index 0, lock cleared. |
| Seek / scrub | Coalesced as today (`content_thread.rs:327-400`). Each applied seek anchors at or after. The UI playhead moves at most one frame from the click. |
| Loop / wrap | No transport loop region exists (`rg -i 'loop_region|transport_loop|loop_brace|loop_enabled|punch|count_in|pre_roll'` finds none in playback). Session loops resolve from the beat (`engine.rs:1854-1890`) and clip loops from video time, so both inherit the grid. If a transport loop is added later, a wrap is a seek to the loop start: index jump plus anchor. |
| Punch / count-in | None exist (same search). Automation gesture punch-in/out samples the grid beat (`automation.rs:139-178`). |
| Session launch quantise | The pending target is an exact beat. It activates on the first grid frame whose beat is at or after it (`engine.rs:1907-1980`). Deterministic. |
| MIDI phantom clips | NoteOn runs before the engine tick (`content_thread.rs:604-605`) and stamps the previous frame's grid beat (`live_clip_manager.rs:462`, `:531`). The 5 ms NoteOff guard stays wall (`:761`). Launch quantise stays inert, a pre-existing issue in CORE_ENGINE_MAP.md section 13 (Honest edges), item 2. |
| Record / arm | Tempo recording, automation recording and MIDI recording stamp grid beats and times. |
| Undo during play | Arrangement edits reconcile next tick (unchanged). Tempo edits follow D11. fps edits follow D10. |
| BPM edit during play | D11: the beat is kept and time moves forward at most one frame. |
| Project load | `initialize` resets the clock; the timer re-anchors (`content_commands.rs:255`). |
| Export start / end | D9. Warm-up ticks pass `frames = 1` on the live clock; then the re-seek and the export clock follow (`content_export.rs:758-796`). |

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Transport time is `origin + index × step` and never accumulated | `transport_clock::tests::time_is_absolute_at_two_hours` (60, 24, f32 29.97: bit-equal to the formula at every frame to 432,000); negative gate `rg -n 'current_time_double \+=|fn advance_time' crates/manifold-playback/src` = 0 |
| A seek lands on the first frame at or after its target, and frames round-trip | `transport_clock::tests::seek_snaps_forward_and_round_trips` |
| The tick after an anchor renders the anchor frame | `transport_clock::tests::anchor_tick_renders_anchor_frame` (play, resume, seek, locate) |
| Same arrangement, same frames, same values, whatever the press phase or jitter | `frame_exact_transport::same_values_per_frame_across_start_phase_and_jitter` (P2, section 5) |
| Sub-frame wall jitter never changes `frames`; a missed deadline skips | `frame_timer::tests::grid_frames_ignore_subframe_jitter`, `grid_counts_missed_deadlines`, `frame_clocked_counts_one` |
| Export time is bit-identical to `origin + k × (1/fps)` | `frame_exact_transport::export_clock_bits_match_today` at 24, 29.97 and 60 fps from an off-grid origin |
| The timer and engine rates never diverge | content-thread test `set_frame_rate_reaches_timer_and_clock`; negative gate: every `timer.set_target_fps(` line in `crates/manifold-app/src` has a matching `engine.set_frame_rate(` within 3 lines (P2 gate command) |
| Corrections never step backward and never bump the epoch | `transport_clock::tests::slew_holds_never_rewinds`; `frame_exact_transport::external_correction_keeps_epoch_and_triggers` |
| Clock quantisation noise never causes a correction at matched tempo | `transport_clock::tests::phase_lock_ignores_clock_quantisation` (24 PPQN ticks, ±1 ms jitter, 2 simulated hours, 0 corrections) |
| A tempo mismatch corrects at the predicted rate | `transport_clock::tests::phase_lock_tracks_tempo_mismatch` (128 vs 128.1: one frame every 21 ± 2 s) |
| External sources cannot stop the grid | negative gate `rg -n 'external_time_sync' crates` = 0 (P3) |
| Video samples, never integrates | negative gate `rg -n 'time_accumulator' crates/manifold-media/src` = 0; `video_renderer::tests::target_frame_follows_transport_time` (P4) |

## 5. Proof plan

Reuse, don't build: the engine tests' `create_engine` / `StubRenderer` / `load_project` helpers (`crates/manifold-playback/tests/engine_tick.rs:6-120`); the live-versus-export pattern in `tests/param_step_clip_edge.rs:200-219`; `FrameTimer::consume_tick_at` for injected instants (`frame_timer.rs:360`); `manifold frame-time` for pacing (`frame_time.rs`); `headless_harness::headless_content_thread` (`crates/manifold-app/src/headless_harness.rs`) for app-level checks. The fixture `Burn V5.manifold` runs at 138 BPM, so a beat is about 26.09 frames: off grid on most beats, which is the hard case.

**The determinism test** — `crates/manifold-playback/tests/frame_exact_transport.rs::same_values_per_frame_across_start_phase_and_jitter`:
1. Project: a tempo change (128 → 141 at beat 32), an automation lane with curve and step segments, a sync LFO (1/16 sine), a free LFO (frame-aligned and not), a clip-edge step envelope, clips starting on off-grid beats, a session launch quantised to 4 beats.
2. Run A: Play from 0, 900 ticks of `frames = 1`.
3. Run B: 37 stopped ticks with random `frames` in 1..=3. Play from 0. Then a seeded random `frames` sequence with 5% twos (dropped frames).
4. Record per frame: index, `time` bits, `beat` bits, active clip ids, every param `value` bit, the session slot state.
5. For every index both runs rendered, the records are equal bit for bit.

The wall-to-frames half is proven separately: the timer grid tests (section 4) feed instants with a random start phase and ±40% of a frame of jitter. The two halves together cover the bead's "different start phase + jittered dt".

**Export regression:** `export_clock_bits_match_today`, plus the existing export tests untouched: the `param_step_clip_edge.rs` export arm, `crates/manifold-app/src/rt_dynamic_export_tests.rs`, and the scoped GPU proofs that `scripts/gpu_scope.py` maps for `content_export.rs`.

**Edge cases:** one test each in `frame_exact_transport.rs`: `tempo_edit_during_play_keeps_beat`, `fps_change_remaps_forward`, `stop_resets_pause_keeps`, `session_launch_on_first_grid_frame`, `noteon_stamps_previous_grid_beat`, `stall_skips_and_stays_in_time`, `seek_while_playing_renders_target_first`.

**L2 artifact:** run `manifold frame-time "<Liveschool Live Show V6 LEDS.manifold>" --frames 3600` twice in paced mode with a new `--transport-log <path>` option. The option extends the existing tool; it does not add a new one. Each log line is `index beat_bits automation_hash`. `diff` of the two logs over shared indices must be empty. Also report the histogram of `frames` per tick and the count of ticks with `frames > 1`.

## 6. Phasing

All of P1–P3 lands as one merge of `feat/frame-exact-clock`. P2 alone leaves external sources half-moved, and Peter performs off main. P4 may land separately after P3.

### P1 — The clock type, CPU only

- **Entry:** branch `feat/frame-exact-clock` at or after 09a44cb4c; `rg -n 'struct TransportClock|transport_clock' crates` = 0.
- **Read-back:** sections 2 and 3.1 of this doc; `crates/manifold-physics/src/clock.rs:278-400`; `driver.rs:167-200`. Restate D1, D4, D6 and D7 and the forbidden moves.
- **Deliverables:** `crates/manifold-playback/src/transport_clock.rs` with the section 3.1 API and private `PhaseLock`; `pub mod transport_clock;` in `crates/manifold-playback/src/lib.rs`; the transport_clock tests named in section 4.
- **Gate:** `CARGO_BUILD_JOBS=4 cargo nextest run -p manifold-playback transport_clock` green; `cargo clippy -p manifold-playback -- -D warnings`. Negative: `rg -n 'Instant|SystemTime' crates/manifold-playback/src/transport_clock.rs` = 0.
- **Forbidden:** wiring into the engine; a rational fps type; an `f64` time field anywhere in the clock.
- **Size:** about 250 lines plus 250 of tests. **Demo:** none — L1. **Test scope:** manifold-playback only.

### P2 — Live and export on the clock

- **Entry:** P1 committed. Re-run the `TickContext` inventory (`rg -n 'TickContext \{' crates -g '*.rs'`, expect 33) and the writer commands in audit item 1.2. A different count means stop and list the new sites.
- **Read-back:** audit items 1.2, 1.3 and sections 3.2–3.4 and 3.6; `content_thread.rs:567-653`; `content_export.rs:560-1022`; `frame_timer.rs` whole. Restate D2–D5 and D9–D11.
- **Seam brief, compiler-driven:** delete `TickContext.dt_seconds`, `pre_render_dt` and `export_fixed_dt` first; the errors are the checklist.
  - Mechanical: a literal with `dt_seconds: Seconds(1.0 / 60.0)` becomes `frames: 1`. Example: `tests/engine_tick.rs:111-117`.
  - Needs the pattern: a non-frame step `d`. If `d × 60` is a whole number, use `frames = d × 60` at 60 fps. Otherwise set the test project's `settings.frame_rate = 1.0 / d` and use `frames = 1`. Example: `tick_n(&mut fine, 50, 0.005)` → frame_rate 200, 50 ticks of 1 frame. `0.295` → frame_rate `1/0.295`. `1000.0` → 60,000 frames at 60 fps.
  - Export (`content_export.rs:761-770`, `:967-973`): `frames` is 0 for frame 0, then 1; `begin_export_clock(start_time, export_config.fps)` replaces `set_export_origin`.
  - `set_time` callers → `locate`; `set_beat` + `sync_time_from_beat` callers → `follow_external_beat` (temporary, deleted in P3, never on main).
  - `ContentThread` timer → `FrameTimer::new_grid`; each `set_target_fps` pairs with `engine.set_frame_rate`.
  - `render_content(…, dt, …)` → `engine.tick_dt()`.
- **Deliverables:** the above; `crates/manifold-playback/tests/frame_exact_transport.rs` with the determinism, export and edge-case tests; the timer grid tests; deletion of `advance_time`, inherent `nudge_time`, `playback_speed`, `on_time_changed`; `--transport-log` on `frame-time`.
- **Gate:** `cargo nextest run -p manifold-playback`; `cargo nextest run -p manifold-app frame_timer content_thread content_export`; clippy on both crates; the scoped GPU proofs for the touched export path (`scripts/gpu_proofs_gate.py`); a `MANIFOLD_RENDER_TRACE=1` paced `frame-time` run with no frame over 20 ms that main does not also show. Negative: `rg -n 'fn advance_time|export_fixed_dt|dt_seconds|pre_render_dt|playback_speed|on_time_changed' crates` = 0.
- **Measured numbers to report:** the L2 transport-log diff (empty), the `frames` histogram over 3600 ticks on the Liveschool fixture, and the count of `frames > 1` ticks.
- **Forbidden:** keeping a dt-driven advance for any caller; clamping `frames`; changing the UI timer; touching sync controllers beyond the mechanical renames.
- **Size:** about 400 lines changed plus 350 of tests and migrations. **Demo:** L2 transport-log diff. **Test scope:** playback and app, scoped.

### P3 — External sources correct the grid

- **Entry:** P2 committed. **Blocking:** none; Q1 has a default. Re-run `rg -n 'external_time_sync|follow_external_beat|nudge_time' crates`.
- **Read-back:** audit item 1.6, D6–D8 and D11, section 3.5; `midi_clock_sync.rs:520-800`; `link_sync.rs` whole; `osc_sync.rs:355-430`; `content_thread.rs:1694-1987`.
- **Deliverables:** section 3.5. `observe_external` wired for all four sources. `external_time_sync` deleted from the engine, `SyncArbiterTarget`, the arbiter, the three controllers and the test mocks (`crates/manifold-playback/tests/osc_timecode.rs:73-88`, `midi_clock_sync.rs:1011-1024`, `osc_sync.rs:624-639`, `sync.rs:250-261`). `follow_external_beat` deleted. Link offset cached on every Link-caused play. One info log per correction (`[Transport] external correction +1 frame (CLK drift 9.1 ms)`), and a warning when corrections exceed one per second (Q1's trigger). Tests `phase_lock_ignores_clock_quantisation`, `phase_lock_tracks_tempo_mismatch`, `external_correction_keeps_epoch_and_triggers`, plus a CLK and a Link scenario in `tests/sync_engine_integration.rs`.
- **Gate:** `cargo nextest run -p manifold-playback`; `cargo nextest run -p manifold-app content_thread`; clippy. Negative: `rg -n 'external_time_sync|follow_external_beat|fn sync_time_from_beat|fn set_beat' crates` = 0.
- **Demo:** L4 on Peter's rig. Ableton sending MIDI clock at the project's tempo, 5 minutes playing: the log shows no correction lines, and LFO motion is smooth where it stepped before. Then Link: start from Ableton after a Stop; MANIFOLD follows position. Click-script in the landing merge commit.
- **Forbidden:** filtering inside the controllers (the lock owns it); re-tuning the 2 s and 0.5 s seek thresholds; timestamping MIDI ticks unless the D7 trigger fires.
- **Size:** about 300 lines plus 250 of tests.

### P4 — Video samples transport time

- **Entry:** P3 landed. Read `video_renderer.rs:500-900`, `engine.rs:2559-2700`, `:2746-2866`.
- **Deliverables:** D12. `ClipRenderer::pre_render` receives each playing clip's target source time from the engine (`compute_video_time`). `time_accumulator` is deleted. Test `target_frame_follows_transport_time`.
- **Gate:** `cargo nextest run -p manifold-media -p manifold-playback`; scoped GPU proofs for video export; negative `rg 'time_accumulator' crates/manifold-media/src` = 0.
- **Demo:** L2. A 30 fps clip in a 60 fps project, played twice: the logged source frame per transport frame is identical.
- **Size:** about 150 lines plus 100 of tests.

### Landing sweep (each landing)

- Update this doc's status line.
- CORE_ENGINE_MAP.md section 4 (Time model & tempo), section 7 (External sync) and section 11 (The threshold table).
- The two stale CORE_ENGINE_MAP items listed in audit item 1.9.
- VSYNC_AND_FRAME_PACING.md, "Content Thread Pacing": add the grid deadline.
- Run `scripts/gen_docs_index.py`.

Total estimate: about 1,100 lines changed plus 950 lines of tests across P1–P4. The bead's 150–300 line estimate covered the engine core only.

## 7. What may look or feel different on stage

Stated plainly:
- **MIDI clock gets smoother.** Today it stair-steps at 24 per beat, with about 12 repeated frames a second at 120 BPM. After P3 every frame advances one frame. Beat-driven visuals will look visibly smoother under CLK. They may also sit up to half a frame earlier than today, because today's value lags the true clock by up to one tick.
- **Play, resume and seek repeat one frame.** The first playing frame is the frame you pressed Play on. That is 16.7 ms of hold at 60 fps.
- **Seeks land up to one frame later** than the click. The playhead line moves by at most one frame; at normal zoom that is invisible.
- **Corrections show as one held or skipped frame.** At a matched tempo they should almost never happen. If Ableton's tempo differs from the project's map, they happen at the mismatch rate: one every 21 s at 0.1 BPM off, four per second at 120 vs 128 (Q1). The log names each one.
- **After a hitch, two content frames go out close together** (section 3.3). Presentation keeps three drawables, so this should not show. The P2 gate measures it.
- **Long stalls jump**, as today: a 2 s freeze resumes 2 s later. The water still slows down rather than jumping, as today.
- **A 120 Hz display looks the same.** Content still runs at project fps and each frame shows twice, as today. The UI playhead still steps once per content frame, as today.
- **MIDI-in latency is unchanged.** Notes are read before the tick, as today. Phantom clips now start on exact grid beats, so a recorded performance replays on the same frames.
- **Paused visuals are unchanged.** Free-running effects keep moving while stopped.

## 8. Decided — do not reopen

1. Integer frame index; time = origin + index × (1 / fps as f64); live origin 0.
2. The content timer uses anchored grid deadlines; the UI timer is unchanged.
3. Stalls skip and stay in time; no cap.
4. Re-anchors snap forward (`ceil` with 1e-7 tolerance) and render the anchor frame first.
5. One tick dt (`frames × step`) for every render consumer, in every transport state.
6. External sources correct, never drive. Half a frame: nothing. Up to 2 frames: one frame per tick, never backward. Beyond that: a jump. Beyond the source's seek threshold: a seek.
7. Link is sampled at the frame deadline; MIDI clock, timecode and AbletonOSC are averaged (α 0.1, +½ frame bias).
8. Export shares the clock with its own origin and stays bit-identical.
9. Wall time stays for telemetry, deadlines, input guards, sync windows, sim load, UI and audio.

## 9. Deferred

| Item | Revive when |
|---|---|
| `frame_count` seeds in effects are tick counts since launch; generators always get 0 (`generator_renderer.rs:922`) | A particle look must repeat exactly across runs |
| Link quantum phase alignment at Play (today: arbitrary offset) | Peter answers Q3 yes |
| Stale live tempo written into the map after Link peers vanish (`content_thread.rs:2083-2103`) | Any beat jump report after Link drops; file a bug bead at landing |
| `SetFrameRate` writes the setting outside `EditingService` (`content_commands.rs:1653-1657`) | The next settings-mutation audit |
| MIDI launch quantise inert (CORE_ENGINE_MAP.md section 13 (Honest edges), item 2) | Its own bead; positions now land on grid beats regardless |
| Audio and video share mach time in Internal mode; the audio device clock may drift ±50 ppm (about 0.36 s per 2 hours) | Audio voices reseek mid-set (200 ms threshold) |
| Timestamped MIDI clock ticks | The D7 trigger fires |

## 10. Open questions for Peter

- **Q1 — Tempo mismatch under an external clock.** Defaulted, non-blocking. When Ableton's tempo differs from MANIFOLD's tempo map, should the live tempo replace the map while Ableton is in charge? **Default:** no. The map stays the truth, the grid corrects one frame at a time, and the log warns when corrections exceed one per second. **Alternative:** the live tempo overrides the map for beat↔time while external. Then clips follow Ableton's tempo exactly, but every beat↔time conversion in the engine reads two sources. **Trigger that makes the default wrong:** Peter sees regular stutter on stage while synced.
- **Q2 — Export anchor.** Defaulted, non-blocking. **Default:** export keeps its origin at the export start, so export output is bit-identical to today. Live and export then match frame for frame only when the export starts on the song grid. **Alternative:** anchor export at song time 0, so live and export always sample the same times. The export's first frame then moves up to one frame later at off-grid starts.
- **Q3 — Link phase at Play.** Deferred. Today MANIFOLD keeps whatever beat offset it had when Play was pressed. Should Play under Link snap MANIFOLD's bar phase to Link's quantum, as Ableton does?
