# Frame-Exact Transport — one frame grid for live play and export

**Status:** IN PROGRESS · 2026-10-14 · P1 (the clock type, CPU only) committed on `feat/frame-exact-clock`, revised after its adversarial review. Owed: P2, landed with P1 as one merge, then Peter's rig test, then P3, then P4. Bead: BUG-gqhep (live-transport-nondeterministic-frame-clock).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

<!-- index: Frame-exact transport clock (BUG-gqhep): impact map of every transport time/beat reader and writer, dt consumer, integrator, external sync source, export and harness; the integer frame-index clock shared with export on the song-time-0 grid, the content-tick grid, on-frame edge sampling, external correction with a persistence rule and timestamped sources, Ableton live-tempo following, phases P1–P4 and the fail-first proof plan. -->

Live play today moves the transport by measured wall time. Each frame samples whatever beat the jittered sum reached, so two plays of the same arrangement give different values on the same frame. Export does not have this problem: it computes time from a frame number. This design gives live play the same kind of clock. Time becomes a function of an integer frame index. Wall time only decides how many frames to step.

Peter's decision, 2026-10-13, quoted from the bead: "One frame-exact transport clock shared with export. Frame grid anchored at song time 0: frame_index N ↔ time N/project_fps; all consumers sample time(frame_index) computed absolutely, never accumulated; real dt only decides how many frames to advance (normally 1). Dropped frame → skip the index (stay in time). Link/MIDI clock/timecode corrections snap to whole frames (nudge the grid, never off-grid time); a correction shows as one held/skipped frame only when drift exceeds half a frame. Seek re-anchors to the grid; loops wrap the index; grid uses project fps, not display rate. Out of scope: audio-triggered clips (depend on live input)."

**For the instrument:** after P1+P2, an LFO, an automation lane or a clip cut lands on the same frame every time the same section plays under MANIFOLD's own clock. After P3 the same holds while Ableton drives, and MIDI clock stops stair-stepping at 24 steps per beat. Video frames and the water simulation are not covered until P4 and never, respectively (D16).

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

Re-derive: `rg -n '\.(current_time_double|current_time|current_beat|current_beat_f64|time_to_timeline_beat|beat_to_timeline_time|advance_time|nudge_time|set_time|set_beat|sync_time_from_beat|seek_to|set_export_origin|transport_epoch)\(' crates -g '*.rs'`. On this base it hits 24 files outside `engine.rs`.

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
- `apply_resolved_tempo` writes a live external tempo into the beat-0 point under Internal authority (`content_thread.rs:2083-2103`), then keeps time and moves the beat (`:2110-2111`). Live tempo is only cleared when there is no project (`content_thread.rs:2033-2036`), so a stale Link tempo can stay after peers vanish. Pre-existing; listed in Deferred.

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

- `frame_clocked`: 24 call sites (`rg -c 'set_frame_clocked\(' crates`). Rechecked against D4. Every harness seeks while stopped or paused, then plays: rt-dynamic held-out (`rt_dynamic_held_out.rs:327-328`), blob and scene-modifier performance, the journeys, corridor acceptance (`corridor_acceptance.rs:657`, `:907`), gap start (`gap_start_probe.rs:71`, `:98`), rt-capture and `frame_time.rs:845`. None seeks while playing, so the D4 hold never fires in a harness. Play advances one frame from the anchor on the first tick, as `frame_clocked` does today. Only the forward snap of a paused seek changes what they see:
  - `scene_modifier_journey/angular.rs:207-211` breaks. It seeks to beats 0.99999 and 1.00001 to straddle a sawtooth reset and asserts `|actual − beat| < 1e-6`. Both snap, and 0.99999 lands on 1.0, after the reset. P2 moves the probe to the grid frames either side of beat 1 (`journey-proofs` feature).
  - `angular.rs:259` (1.00001), `consolidation.rs:93`, `:170` (0.1), `periodic.rs:229`, `:289` and `scene_modifier_journey.rs:561`, `:608` (4.25) compare captures taken at the same snapped beat, or compute expectations from `current_beat_f64()`. They keep working. P2 reruns them once.
  - `gap_start_probe.rs:105` makes its own `beat >= start_beat` compare. D14 makes the engine's beat land at or past on-frame edges, so the probe and the engine agree.
- `manifold frame-time` (`crates/manifold-app/src/frame_time.rs:1-33`) measures production pacing headless. It is the oracle for the timer change.
- `headless_content_thread` opens the test GPU device (`crates/manifold-app/src/headless_harness.rs:26`). App-level tests built on it are single queued GPU runs, not CPU proofs.
- `TickContext` struct literals: 33 sites (`rg -n 'TickContext \{' crates -g '*.rs'`): engine tests 3, `tests/engine_tick.rs` 15, `tests/param_step_clip_edge.rs` 3, `tests/sync_engine_integration.rs` 2, `tests/session_mode.rs` 1, `tests/trigger_source_switch.rs` 1, `content_thread.rs` 1, `content_export.rs` 2, `content_pipeline/trigger_lane_force_tests.rs` 2, `bug219_verify.rs` 1, `import_responsiveness_verify.rs` 1, `ui_snapshot/script.rs` 1. Plus `TickContext::default()` in engine unit tests.
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

## 2. Decisions

**D1 — One clock type, integer index, absolute time.** `TransportClock` holds an `i64` frame index and a step. Time is always `index as f64 × step`, with `step = 1.0 / fps as f64` from the `f32` fps. Live and export both count from song time 0 (D17), so there is no origin. Nothing ever adds to time. The expression is today's export expression with the origin at 0.
Rejected: an exact rational `(num, den)` type. Its benefit is 7.4 ms over 2 hours at 29.97. Rejected: a time accumulator snapped at read time. It reintroduces the path dependence this bead removes.

**D2 — The content tick runs on an anchored grid, counted by instant comparison.** Deadline n is `anchor + Duration::from_secs_f64(n as f64 × step)`. The timer counts `frames` as the number of deadlines with `deadline(n) <= now` since the last tick. It decides by comparing `Instant`s, never by dividing elapsed time. After a long stall it estimates n from elapsed time only to bound the search. The comparison against `deadline(n)` and `deadline(n + 1)` decides. Normally `frames` is 1. The anchor resets on load and on an fps change. With `frame_clocked`, `frames` is always 1. The UI thread's timer keeps today's free-running deadline.
The counting lives in `FrameGrid` (`crates/manifold-playback/src/frame_grid.rs`), which holds the `Instant`s so the clock stays pure. The content thread's `FrameTimer` wraps it (section 3.3). `FrameGrid` and the clock share `step_for_fps` and the fps rule (D10), so their steps agree bit for bit. Deadline indices stop at 2^32, so no deadline can overflow an `Instant`.
Why anchored: with a free-running deadline every late microsecond is kept, and counting grid frames against it skips a transport frame about once a minute at 5 µs lateness per tick. Computed. With anchored deadlines, sub-frame jitter never changes `frames`.
Why compare instants: a division near a boundary can disagree with the timer's own deadline by one ulp, and the timer would then wait for a deadline that the count already passed.

**D3 — Stalls skip and stay in time.** After a stall of any length, the transport skips the deadlines passed. No cap. The 5 s GPU surface timeout (`content_thread.rs:485`, `:498-502`) is the longest stall the content thread itself creates. The physics clock keeps its own rule, with at most two Sim Rate intervals per frame and the rest discarded (LIVE_SIM_CLOCK_DESIGN.md section 7 (Decided — do not reopen)). The transport keeps time while the water goes slow-motion.

**D4 — Re-anchors snap to the grid. Only a seek while playing holds.**
- Snap rule: `index_at_or_after(t) = ceil(t / step − 1e-7)`, the tolerance `Driver::evaluate_frame_aligned` uses. A scripted round trip at nine rates over frames 0–5,000 and 431,000–433,000 had zero mismatches.
- Play and resume: anchor at or after the current time and advance normally. The first playing tick moves `frames` from the anchor, exactly as a `frame_clocked` harness does today. The paused frame was already on screen, so nothing repeats.
- Seek while stopped or paused: snap forward. The next tick renders it because the transport is not moving.
- Seek while playing: snap forward and hold one advance, so the next tick renders the target frame. Without the hold the first frame after a seek to a clip start would be one frame into the clip.
- Under external authority (P3), a re-anchor goes to the frame nearest the external target, with no hold. A hold would start every external locate half a frame behind on average.
Why "at or after": a clip spans `[start, end)`. The first frame that shows a clip starting at `s` is `ceil(s / step)`.
Rejected: nearest rounding for user seeks, because a seek to a clip start could land just before it and show black.

**D5 — One tick dt for everything that renders.** `tick_dt = frames × step`, in every transport state. It feeds generators, effects, the compositor, modulation's audio followers, audio triggers and video `pre_render`. Export frame 0 has `frames = 0`, so `tick_dt = 0` as today.
Rejected: a zero dt while paused. Paused graphs would freeze.

**D6 — External sources correct the grid; they never drive it. A persistence rule stops flapping.** The grid always advances by `frames`. Each external source supplies a target time for the frame about to render (D7). The clock keeps a drift estimate `e`, measured in frames (target minus grid time).
- **The contract on sources:** after smoothing, a source's noise stays under ¼ frame. The projections in D7 deliver it. P3 measures each source against it.
- At each anchor, on any authority change (CLK ↔ AbletonOSC ↔ Link ↔ timecode ↔ Internal) and on Stop, the estimate is cleared. The first sample after a clear seeds `e` directly. It is not averaged in from 0.
- **Persistence.** A correction in either direction needs `|e| > ½ + h` on K consecutive observations on the same side, with h = ¼ and K = 6. A sample back inside `½ + h`, or on the other side, restarts the count. Every correction restarts it, so each frame of a slew needs its own K observations. Nothing else carries over from a correction.
- Why it cannot flap inside the contract: a skip needs the true offset above ½ (¾ less ¼ of noise). The residual after the skip is then above −½, and a hold would need it below −½.
- Why K: it absorbs noise beyond the contract. The P1 sweep (2026-10-14) ran one simulated hour per case over noise amplitude, true offset (−1.5 to 1.5 frames) and start phase. At a half-frame offset with uniform ±0.3-frame exact-source noise, K = 1 gives 18,150 corrections an hour, K = 3 gives 128, K = 4 gives 12, and K = 6 gives 1 with no change of direction. At K = 6, ±0.35 gives 4 an hour. K = 8 holds to ±0.4 for two more frames of latency. h = 0.3 also works but moves Peter's half-frame rule further out. Coarse noise past about ±0.5 frame raw (smoothed peaks over ¼) flaps at any K; the contract covers that, not K.
- Cost: a real offset is corrected K observations after it appears. That is 100 ms for Link at 60 fps, and 6 samples on a coarse source.
- `½ + h < |e| ≤ 2½`: one frame per correction, in the sign of `e`. Positive adds one frame (a skip). Negative removes one frame from this tick's advance (a hold). The index never moves backward on a slew. A hold that falls due on a tick with no frame to remove (the seek-hold tick, or `frames = 0`) waits for the next tick and keeps its estimate.
- `2½ < |e|` below the source's seek threshold: a jump by `round(e)` frames. Every jump then moves at least 3 frames, so a backward jump never nets a one-frame rewind against the tick's own advance. With the threshold at 2, drift from −2.01 to −2.5 jumped −2 and moved the index back one frame. The rule equals "slew while the rounded drift is under 3" everywhere except exactly 2½, where it slews, the safer side.
- After any correction of `c` frames, `e −= c`. The filter state carries the correction, so the next ticks don't see the old drift and correct again.
- **Coarse locates.** A coarse sample more than 2 frames from `e` is not averaged in. If the next sample is also more than 2 frames out and within 1 frame of the first, it re-seeds `e`, and the locate becomes one jump. A single outlier changes nothing. Averaging a 100-frame step at α = 0.1 instead gave 19 jumps in a row.
- At or above the source's seek threshold, or on a transport jump: the existing seek path.
- Bad input follows D18.

Corrections and jumps do not bump `transport_epoch` and do not run seek side effects. A backward jump trips the backward-move guard in `sync_clips_to_time` (`engine.rs:1633-1642`), which resets trigger delivery, as today's backward nudges do. The clock reports each correction and the drift that caused it (`last_correction`, `last_correction_drift`) for P3's log line.
What a jump of more than 2½ frames does:
- Forward: the clip window covers `[previous beat, landing beat]` (`engine.rs:1643-1652`). Quantised session launches whose target lies inside the jump activate on the landing frame, as after a dropped-frame skip.
- Backward: launches that already fired stay fired, and pending targets ahead stay pending.
- Either direction: a net move of 3 frames or more trips the temporal reset rule (audit item 1.3), so feedback and trails restart on that frame. Today's nudges do the same.
Why "never backward on a slew": a one-frame backward step resets trigger delivery every time. Why jump at all: slewing 1.9 s of drift a frame at a time would play double speed or freeze for 1.9 s.

**D7 — Each source is sampled at the frame deadline, with its own bias removed.** A source's target is its position projected to this frame's deadline, not the last value it happened to deliver.
- **Link:** `beat_at_time(deadline_micros, quantum)`, with the deadline mapped into Link time by reading `link.clock_micros()` and `Instant::now()` back to back. Exact; no averaging (α = 1), the persistence rule still applies. ⚠ VERIFY-AT-IMPL: `rusty_link`'s `beat_at_time` takes an arbitrary micros value and `clock_micros` shares the host clock (`link_sync.rs:96-131`, rusty_link `SessionState` docs).
- **MIDI clock:** the midir callback stores each tick's arrival time in a second `AtomicU64`. The writer stores the timestamp, then the packed state. The reader reads the state, then the timestamp, then the state again, and retries if the state changed. Target beat = tick beat + (deadline − arrival) × estimated BPM / 60. The BPM comes from tick arrival intervals. ⚠ VERIFY-AT-IMPL: midir's timestamp base on macOS. If it is CoreMIDI host time, convert it once to `Instant`. If not, stamp `Instant::now()` in the callback (CoreMIDI callback latency is well under a millisecond).
- **OSC timecode:** P3 changes the M4L sender to send unquantised song seconds instead of timecode frames. The arrival `Instant` is stored with each message. Target time = song seconds + (deadline − arrival).
  - Why the sender changes: a sender that quantises to its own timecode frames on a free-running metro beats against the grid. At a 40.1 ms metro against 25 Hz timecode the error is a slow sawtooth of ±1.2 frames, beyond the D6 contract, and the P1 clock makes 149 corrections in 10 minutes whatever the persistence. No filter rule fixes a slow periodic error; removing the quantising does.
  - The P3 test sweeps the send period (free-running metros such as 40.0, 40.1, 33.3, 33.4, 20 and 50 ms) and the start phase, never one fixed phase. P1's model of the new sender gives 0 corrections over 2 hours on that sweep.
- **AbletonOSC nudges:** target beat = reported beat + (RTT/2 + deadline − arrival) × BPM / 60, using the relay's own RTT estimate (`transport_sync.rs:34-37`).
- Coarse sources (MIDI clock, timecode, AbletonOSC) still average with α = 0.1. That covers the jitter left after projection.
- Seek thresholds stay as today: MIDI clock 2.0 s (`midi_clock_sync.rs:782`), OSC timecode 0.5 s (`osc_sync.rs:405-428`), AbletonOSC 2.0 s (`transport_sync.rs:31`).

**D8 — Under CLK or Link, the beat follows Ableton's live tempo. Under Internal, the map is the truth.** Peter's ruling, 2026-10-13.
- Time is always the grid.
- Under Internal, OSC timecode and AbletonOSC authority: `beat(N) = map(time(N))`. Timecode sends time, and the AbletonOSC relay has no live tempo channel today.
- Under CLK or Link authority, the beat comes from a **live-tempo segment** `(anchor_time, anchor_beat, bpm)`: `beat(N) = anchor_beat + (time(N) − anchor_time) × bpm / 60`. It is computed from the index each frame, never accumulated.
- The anchor is a time, not a frame index. An fps change (D10) or the export grid (D9) remaps indices, and an index anchor would then sit at a different time and jump the beat: anchored at frame 600 at 120 BPM with the playhead at 20 s, `set_fps(30)` read beat 20 instead of 40. The anchor time is always a grid time, so the beat is still a function of the index.
- The segment re-anchors at the current frame, keeping the beat continuous, at each correction and whenever the source's BPM estimate moves by more than a small epsilon (named and tested in P3). An Ableton tempo ramp is a run of short segments, each anchored where the last left off.
- Every beat↔time conversion in the engine goes through one converter that is either the map or the segment. That covers the 13 `TempoMapConverter::` calls and the helpers at `engine.rs:2133-2170` and `:3266`. Clip edges, video in-points and session targets therefore agree with the beat on screen.
- When authority returns to Internal, the beat is kept and the index remaps to the first frame at or after `map⁻¹(beat)`, as a tempo edit does (D11). The position never jumps on hand-back.

**D9 — Export shares the clock, set before anything moves.**
- `begin_export_clock(fps)` runs before the first seek (`content_export.rs:717`). The two seeks, the warm-up ticks and the frame loop all run on the export grid at export fps.
- Export frame k sets the index to `index_at_or_after(start) + k` on the song-time-0 grid at export fps (D17), so live and export sample identical times.
- In export mode the tick sets the index regardless of external sync, so an export under CLK or timecode advances (audit item 1.1).
- Bit-identity with old export is not a goal. Live equals export is the requirement. D14 changes on-frame edge results in export as well as live.
- The end of export restores the live rate and seeks to the restore time as today (`content_export.rs:427`, `:497`).

**D10 — One source of truth for fps.** `project.settings.frame_rate` is the only fps. Each tick, outside export, the engine compares its clock fps with it and remaps when they differ. It keeps the current time and moves the index to the first frame at or after it, with no hold. The content thread then retargets the timer from `engine.frame_rate()` when the two differ. The four `timer.set_target_fps` copies in `content_commands.rs` are deleted. The rt-capture override becomes a project setting written before load.
The clock and the tick grid accept 1 to 1000 fps through one rule (`sanitize_fps` in `transport_clock.rs`). Anything else, including NaN and infinity, runs at 60 with a one-time warning naming the bad value, so a corrupt setting never stops the transport.

**D11 — Tempo edits during play keep the musical position.** Under Internal authority, after a map or BPM edit, the beat is kept and the index remaps to the first frame at or after `map'(beat)`, with no hold. Under CLK or Link the segment governs the beat, so a map edit changes nothing on screen until hand-back (D8). Under OSC timecode, time is the master, so the index is kept and the beat re-derived.

**D12 — Video picks its frame from transport time.** Each tick, a playing clip's target source frame is `floor(video_time × video_fps)` from `compute_video_time` (`engine.rs:2559`). It requests that frame when it differs from the one shown and seeks when the gap exceeds the existing 3-frame rule. Both integrators go: `time_accumulator` and `playback_time += dt × rate` (`video_renderer.rs:812-813`). Decode stays asynchronous: a late decode holds the previous frame. P4 owns this.

**D13 — What stays on wall time.** The content timer's own telemetry, `realtime_now` and every deadline built on it (audit item 1.3), the sim late-frame rule, MIDI input guards, sync-controller windows, the UI thread, audio voices and audio analysis. None of these may read the transport frame index.

**D14 — A frame's beat is sampled a ten-millionth of a frame late.** The engine computes the one beat per frame as `beat_at(time(index) + 1e-7 × step)`. Time itself stays exact. An edge that sits exactly on frame k then always counts as reached on frame k. That covers clip edges, automation steps, session quantise targets and one-shot expiry, with one rule at one site. A nudge of 1e-7 frames is 1.7 ns at 60 fps, 2,000 times the worst rounding error (3.6e-12 beats) and far below anything visible. Scripted over every on-frame 1/16 edge at 60, 50, 30, 25 and 24 fps and 120–180 BPM: the matching frame rule `ceil(edge_time / step − 1e-7)` selects the right frame for all of them. It moves no off-frame edge.
Rejected: comparing each edge as an integer frame against the index. It is the same rule, but it would put an fps into `Layer`'s beat queries and into every other comparer.

**D15 — Link offset is sampled at the anchor deadline and aligned to the quantum.** `cache_link_beat_offset` samples `beat_at_time` at the anchor frame's deadline, not the last poll, and pairs it with the anchor frame's beat. It also runs on every transition into Playing that Link caused, which closes the NaN gap in audit item 1.6. On a Link-initiated Play, per Peter's ruling of 2026-10-13, the offset rounds to a whole multiple of Link's quantum. MANIFOLD then locates to `link_beat − offset` at the nearest frame, so its bar lines up with Link's bar. The play start moves by at most half a quantum.

**D16 — What the determinism claim covers.** Bit-equal per frame index on two runs, whatever the press phase, jitter or stalls: time, beat, clip membership, automation, LFOs and drivers, session launches, one-shot expiry, and every DERIVED reader in audit item 1.2. Integrators fed tick dt are bit-equal only when the two runs dropped the same frames (audit item 1.3). Video frames: not until P4. Physics and liquid: never. The sim's live cap and reanchor read wall load (`clock.rs:125-130`, `:380`), so water under load differs between runs. That is LIVE_SIM_CLOCK_DESIGN's choice of slow motion over stutter, and this design does not reopen it.

**D17 — Export runs on the song-time-0 grid.** Peter's ruling on Q2, relayed 2026-10-14. Export frame k is the first export-grid frame at or after the export start, plus k. Live and export then sample identical times for the same song moment. An export started off the grid begins up to one frame after its marker: at 128 BPM and 60 fps, half a frame on every odd bar.
Rejected: frame 0 exactly on the marker. Export would then sample times live play never renders. Putting both on the marker would mean anchoring the live grid wherever Play was pressed, which is the non-determinism this bead removes.
Consequence: the clock has no origin (D1). `begin_export(fps)` and `end_export(fps)` change the step and keep the current time.

**D18 — Bad input never moves or breaks the clock.** `locate`, `seek_playing`, `locate_nearest` and `observe_external` ignore non-finite times. Absurd finite times clamp the index to ±2^52, where `index × step` is still exact. Drift clamps to ±10^6 frames; every source seeks long before that. Index sums saturate, and the tick grid's deadline indices stop at 2^32, so no arithmetic overflows and no `Duration` conversion sees a bad value.

## 3. Design

### 3.1 `TransportClock` — `crates/manifold-playback/src/transport_clock.rs` (new)

Owned by `PlaybackEngine`, content thread only. Pure: no `Instant`, no allocation, no tempo map. It lives in playback because the engine owns it and export drives the engine.

Precedents: `manifold_physics::clock::SimulationClock` (`crates/manifold-physics/src/clock.rs:278-400`) for absolute time from an integer count. `Driver::evaluate_frame_aligned` (`driver.rs:189-200`) for the lattice tolerance.

```rust
/// The frame's beat is taken this many frames after its time (D14); also the seek-snap tolerance.
pub const LATTICE_TOLERANCE_FRAMES: f64 = 1e-7;

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
    lock: PhaseLock,      // external drift estimate and persistence count (D6)
    last_advance: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Correction { #[default] None, Skip, Hold, Jump(i64) }
impl Correction {
    pub fn frames(self) -> i64; // 0, +1, -1 or n
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smoothing { Exact, Coarse } // α = 1 (Link) or α = 0.1 (D7)

impl TransportClock {
    pub fn new(fps: f32) -> Self;
    pub fn time(&self) -> Seconds;
    pub fn time_at(&self, index: i64) -> Seconds;
    /// time(index) + 1e-7 * step: the instant the frame's beat is taken at (D14).
    pub fn beat_sample_time(&self) -> Seconds;
    pub fn index(&self) -> i64;
    pub fn step(&self) -> Seconds;
    pub fn fps(&self) -> f32;
    pub fn index_at_or_after(&self, t: Seconds) -> i64; // ceil(t/step - 1e-7)
    pub fn index_nearest(&self, t: Seconds) -> i64;
    /// Play, resume, paused seek, tempo edit: snap forward, no hold. Non-finite t ignored.
    pub fn locate(&mut self, t: Seconds);
    /// Seek while playing: snap forward and hold the next advance (D4).
    pub fn seek_playing(&mut self, t: Seconds);
    /// External-authority locate: nearest frame, no hold (D4).
    pub fn locate_nearest(&mut self, t: Seconds);
    pub fn reset(&mut self); // index 0, no hold, lock cleared
    /// Once per playing tick: hold, then `frames`, then at most one correction.
    /// Returns the net move, negative only for a backward jump.
    pub fn advance(&mut self, frames: u32) -> i64;
    pub fn last_advance(&self) -> i64;
    pub fn set_fps(&mut self, fps: f32); // keeps time, snaps forward (D10)
    pub fn begin_export(&mut self, fps: f32); // song-time-0 grid at export fps (D9, D17)
    pub fn end_export(&mut self, live_fps: f32);
    pub fn set_export_frame(&mut self, first_index: i64, k: u32); // clears a pending hold
    /// P3: one target per source sample, already projected to this frame's deadline (D7).
    pub fn observe_external(&mut self, target: Seconds, smoothing: Smoothing);
    /// Anchor, authority change, Stop (D6).
    pub fn clear_external(&mut self);
    pub fn last_correction(&self) -> Correction;
    /// The smoothed drift that triggered `last_correction`, for P3's log line.
    pub fn last_correction_drift(&self) -> Seconds;
}

/// P3, CLK and Link only (D8). Pure; the engine's converter holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveTempo { anchor_time: f64, anchor_beat: f64, bpm: f64 }
impl LiveTempo {
    pub fn new(anchor_time: Seconds, anchor_beat: Beats, bpm: Bpm) -> Self;
    pub fn bpm(&self) -> Bpm;
    pub fn beat_at(&self, t: Seconds) -> Beats;
    pub fn time_at(&self, beat: Beats) -> Seconds;
    /// Re-anchor at `at`, keeping the beat continuous.
    pub fn reanchor(&mut self, at: Seconds, bpm: Bpm);
}
```

`PhaseLock` is private: the estimate `e`, a seeded flag, the persistence count and its side, the pending coarse outlier, and the last correction with the drift that caused it (D6). The engine owns the conversion from a source's beat target to a target time, through the active converter (D8). The clock never sees a beat.

The tempting wrong turn: you will want to keep `current_time_double` and "snap it" after each `+= dt`. No. That is still an accumulator. The index is the state; time is computed from it.

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
| fields `current_time_double`, `current_time`, `playback_speed`, `export_origin_seconds`, `on_time_changed` | `clock: TransportClock`; `tempo: TempoSource` (map, or `LiveTempo` in P3); `current_beat: f64` stays a cache, refreshed after every clock change from `beat_sample_time()` |
| `advance_time(dt)`, inherent `nudge_time(t)` | deleted; `tick_playing` calls `clock.advance(ctx.frames)` |
| `set_time(t)` | `locate(t)` |
| `seek_to(t)` | unchanged contract; `clock.seek_playing(t)` while playing, else `clock.locate(t)` |
| `set_beat(b)` + `sync_time_from_beat()` | P2: `follow_external_beat(b)` = `clock.locate_nearest(map(b))`, marked stopgap. P3: deleted |
| `set_export_origin(t)` + `export_fixed_dt` | `begin_export_clock(fps)` before the first seek, `end_export_clock()`; in export mode the tick calls `set_export_frame` whatever the sync flag |
| `external_time_sync` and its setters (engine and `SyncArbiterTarget`) | P2 keeps them as the stopgap; deleted in P3 |
| — | `transport_frame() -> i64`, `tick_dt() -> Seconds` (D5), `frame_rate() -> f32` (D10), `rederive_beat()` |
| each tick, outside export | `if project fps != clock.fps() { clock.set_fps(project fps) }` (D10) |

`tick_dt()` is what the content thread passes to `render_content` in place of wall dt, and what `filter_ready_clips`, modulation and audio triggers read in place of `ctx.dt_seconds` / `ctx.pre_render_dt`.

### 3.3 `FrameTimer` — `crates/manifold-app/src/frame_timer.rs`

```rust
pub struct TickFrames { pub frames: u32, pub wall_dt: Seconds }

impl FrameTimer {
    pub fn new(target_fps: f64) -> Self;      // free-running, UI thread, unchanged
    pub fn new_grid(target_fps: f64) -> Self; // content thread
    pub fn consume_frames_at(&mut self, now: Instant) -> TickFrames;
    pub fn consume_frames(&mut self) -> TickFrames; // = consume_frames_at(Instant::now())
    pub fn consume_tick(&mut self) -> f64;    // UI thread, unchanged
}
```

`new_grid` holds a `FrameGrid` (`crates/manifold-playback/src/frame_grid.rs`, built in P1), and `consume_frames_at` returns its `consume_at` count. `deadline(n) = anchor + Duration::from_secs_f64(n as f64 × step)`. `consume_at` advances `n` while `deadline(n + 1) <= now` (D2). After a stall it starts the search from an estimate and corrects by comparison in both directions. `frames = n − n_last`, at least 1, because production waits for `deadline(n_last + 1)`. `frame_clocked` gives `frames = 1`. `missed_ticks` becomes `frames − 1`. `resume_after_load` and `set_target_fps` re-anchor at now. Wall telemetry is unchanged.

Consequence: after a tick that is late by less than a frame, the next deadline is closer than a full period, so two content frames go out back to back. That is how fixed-step loops behave. The sim's late-frame rule (LIVE_SIM_CLOCK_DESIGN.md section 8 (Resolved decisions and deferred scope)) may see one extra late frame after a hitch. The P2 gate measures it.

### 3.4 Content thread

- `tick_frame(state_tx)` becomes `tick_frame_at(Instant::now(), state_tx)`. `tick_frame_at` is the seam the app-level test drives (section 5). It calls `consume_frames_at(now)` and builds `TickContext { frames, realtime_now, frame_count }`, then calls `render_content(…, self.engine.tick_dt().0, …)`.
- `ContentThread` builds its timer with `FrameTimer::new_grid` (`app.rs:2556`, `headless_harness.rs:92`).
- After each engine tick: `if self.timer.target_fps() != self.engine.frame_rate() { self.timer.set_target_fps(…) }` (D10). The copies at `content_commands.rs:1358`, `:1436`, `:1492` and `:1656` are deleted. `rt_capture.rs:483`, `:498` write `settings.frame_rate` on the project before load instead.
- `derive_external_beat` (`content_thread.rs:1922-1968`): P2 calls `follow_external_beat`. P3 replaces it with `observe_external`.
- Play aligned to MIDI clock (`content_commands.rs:900-910`): P2 `engine.locate(map(clock beat))`; P3 `locate_nearest` at the projected clock beat.

### 3.5 External sources (P3)

| Source | Today | After |
|---|---|---|
| MIDI clock | `set_external_time_sync` (`midi_clock_sync.rs:591`); nudge and seek (`:777-797`); per-frame beat copy (`content_thread.rs:1951-1962`) | Play/pause unchanged. Tick timestamps (D7). Target = projected beat through the live-tempo segment (D8), `Coarse`. Seek unchanged (2 s and SPP jumps). Beat copy deleted. |
| Link | per-frame beat copy (`content_thread.rs:1928-1941`) | `Exact` target at the frame deadline through the segment, while the existing gates hold. Offset per D15. |
| OSC timecode | `set_external_time_sync` (`osc_sync.rs:373`); nudge and seek (`:405-428`) | `Coarse` target per message, projected (D7). Map beats. Seek unchanged (0.5 s). |
| AbletonOSC relay | nudge / seek actions (`content_thread.rs:1888-1909`) | NudgeBeats becomes a `Coarse` projected target (D7). SeekBeats unchanged (2 s, `transport_sync.rs:31`). |
| Arbiter | `SyncArbiterTarget::nudge_time`, `set_external_time_sync` (`crates/manifold-playback/src/sync.rs:23-27`, `:181-228`) | `nudge_time` becomes `observe_external` with the gate unchanged. An authority change calls `clear_external` and swaps the converter (D6, D8). The gate-matrix tests (`sync.rs:318-357`) move with it. |

### 3.6 Edge cases

| Case | Behaviour after |
|---|---|
| Play / resume | Snap forward from the current time, then advance normally. No repeated frame. |
| Pause | Index kept. |
| Stop | Index 0, lock cleared. |
| Seek / scrub while stopped | Coalesced as today (`content_thread.rs:327-400`). Snap forward. The playhead moves at most one frame from the click. |
| Seek while playing | Snap forward, hold one advance: the target frame is shown. |
| External locate (P3) | Nearest frame, no hold, lock cleared and re-seeded. |
| Jump over 2½ frames | D6: forward jumps activate launches inside the window, backward jumps reset trigger delivery, 3+ frames reset temporal effects. |
| Loop / wrap | No transport loop region exists. Session loops resolve from the beat (`engine.rs:1854-1890`), clip loops from video time. A future transport loop wraps as a seek. |
| Punch / count-in | None exist. Automation gesture punch-in/out samples the grid beat (`automation.rs:139-178`). |
| Session launch quantise | Activates on the first frame whose sampled beat is at or after the target (`engine.rs:1907-1980`). D14 makes on-frame targets land on their frame. |
| MIDI phantom clips | NoteOn runs before the engine tick (`content_thread.rs:604-605`) and stamps the previous frame's grid beat (`live_clip_manager.rs:462`, `:531`). The 5 ms NoteOff guard stays wall (`:761`). |
| Record / arm | Tempo, automation and MIDI recording stamp grid beats and times. |
| Undo during play | Arrangement edits reconcile next tick. Tempo edits follow D11. fps edits follow D10 through the per-tick compare. |
| Project load | `initialize` resets the clock; the timer re-anchors (`content_commands.rs:255`). |
| Export start / end | D9: export clock first, then seeks, warm-up and frames on the export grid. |
| Export under CLK or timecode | Advances (D9). Today it freezes. |
| Authority change | Lock cleared; converter swapped; hand-back to Internal keeps the beat (D8). |

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Transport time is `index × step`, never accumulated | `transport_clock::tests::time_is_absolute_at_two_hours` (60, 24, f32 29.97, bit-equal to the formula up to frame 432,000); negative gate `rg -n 'current_time_double \+=|fn advance_time' crates/manifold-playback/src` = 0 |
| On-frame edges land on their frame | `transport_clock::tests::on_frame_edges_land_on_their_frame`: every 1/16-beat edge at 120–180 BPM and 60/50/30/25/24 fps, first 2 hours, through `TempoMapConverter`. The frame whose `beat_sample_time` beat first reaches the edge equals the exact rational edge frame, and off-frame edges are unmoved. It must fail on today's sampling at 150 BPM. |
| Seeks snap forward and round-trip | `transport_clock::tests::seek_snaps_forward_and_round_trips` |
| Only a seek while playing holds | `transport_clock::tests::only_playing_seek_holds` (play, resume, paused seek, playing seek, external locate) |
| Same arrangement, same values per index, whatever the press phase, jitter or stalls | `frame_exact_transport::same_values_per_index_across_phase_jitter_and_stalls` (playback, CPU) and `frame_exact_app::same_values_per_index_through_content_thread` (app, one queued GPU run). Both committed red first (section 5). |
| Sub-frame jitter never changes `frames`; a missed deadline skips | `frame_grid::tests::grid_frames_ignore_subframe_jitter`, `grid_counts_missed_deadlines`, `grid_count_matches_deadline_comparison` (P1); `frame_timer::tests::frame_clocked_counts_one` (P2) |
| The tick grid and the clock share one step | `frame_grid::tests::grid_and_clock_share_the_step`: both checked against D1's written formula and the exact `n / fps`, not against each other |
| Export samples the same grid as live | `transport_clock::tests::export_frames_sit_on_the_song_grid` (P1, D17); `frame_exact_transport::export_frames_match_live_indices` and `export_clip_inside_first_frame` (24 fps export of a 60 fps project, a clip starting between the export start and the next live-grid frame: no start before its own export frame) |
| An export frame leaves no seek hold | `transport_clock::tests::export_frame_clears_a_pending_hold` |
| Export advances under external sync | `frame_exact_transport::export_under_external_sync_advances` |
| fps has one source and one sanitising rule | `transport_clock::tests::fps_outside_one_to_a_thousand_falls_back_to_sixty`, `frame_grid::tests::bad_fps_falls_back_without_panicking` (P1); `content_thread::tests::timer_and_clock_follow_project_fps` (load, SetFrameRate, undo, redo) |
| Bad input never moves or breaks the clock (D18) | `transport_clock::tests::non_finite_and_absurd_input_is_safe` (NaN, ±inf, ±1e300 to every mutator; `i64` extremes to `set_export_frame`; `u32::MAX` advances) |
| Corrections never flap, never rewind on a slew, never get stuck, never bump the epoch | `transport_clock::tests::phase_lock_hysteresis_no_flap` (start phase swept over [−½, ½] in 1/64 frame steps, MIDI 90–180 BPM and an exact source, offsets 0, ±0.6, ±0.8: at most one correction each); `phase_lock_bounded_under_noise` (noise past the contract, ±0.3 exact and ±0.5 coarse, offsets −1.5 to 1.5 in 1/16 steps, two start phases, one hour each: at most two corrections over need; fails at K = 1); `correction_needs_a_persistent_streak`; `reverse_after_skip_is_never_stuck`; `slew_never_rewinds` (to −2.5); `jumps_beyond_two_and_a_half_frames`; `due_hold_waits_when_there_is_no_frame_to_remove`; `coarse_locate_is_one_jump` (3, ±24, 100 frames); `coarse_outlier_is_ignored`; `correction_subtracts_from_estimate`; `authority_change_clears_and_reseeds`; `frame_exact_transport::external_correction_keeps_epoch_and_triggers` |
| Timestamped sources cause no corrections at matched tempo | `transport_clock::tests::projected_sources_hold_phase`: MIDI 24 PPQN with ±1 ms arrival jitter at 120 and 174 BPM, and the P3 song-seconds sender at send periods 40.0, 40.1, 33.3, 33.4, 20 and 50 ms with three start phases, drift −½ to ½, 2 simulated hours each, 0 corrections. The projection uses the true tempo and send time; P3 adds its own BPM estimate and arrival stamps and re-measures. |
| Under CLK/Link the beat follows the live tempo | `transport_clock::tests::live_tempo_is_absolute_and_continuous` and `live_tempo_survives_grid_changes` (P1); `frame_exact_transport::clk_tempo_ramp_follows_without_corrections` (120 → 128 BPM ramp over 16 beats: beat error under ¼ frame, corrections ≤ 1) |
| External sources cannot stop the grid | negative gate `rg -n 'external_time_sync' crates` = 0 (P3) |
| Video samples, never integrates | negative gate `rg -n 'time_accumulator|playback_time \+=' crates/manifold-media/src` = 0; `video_renderer::tests::target_frame_follows_transport_time` (P4) |

## 5. Proof plan

Reuse, don't build:
- the engine tests' `create_engine` / `StubRenderer` / `load_project` helpers (`crates/manifold-playback/tests/engine_tick.rs:6-120`);
- the live-versus-export pattern in `tests/param_step_clip_edge.rs:200-219`;
- `FrameTimer::consume_tick_at` for injected instants (`frame_timer.rs:360`);
- `manifold frame-time` for pacing;
- `headless_content_thread` for app-level checks.

**Fail first.** The first commit of P2 adds both determinism tests against today's `TickContext` and today's `tick_frame`, and records them red in the commit message.
- The playback test drives the engine through one test helper, `drive(engine, instants)`. Today the helper turns consecutive instants into `dt_seconds`. After P2 it turns them into `frames` with the D2 rule. The helper is the only part of the test that changes. The test body and its assertions stay as committed red.
- The app test needs `tick_frame_at` (section 3.4). That seam is the commit's only production change and moves no behaviour.

**Playback test** — `crates/manifold-playback/tests/frame_exact_transport.rs::same_values_per_index_across_phase_jitter_and_stalls`:
1. Project:
   - a tempo change (128 → 141 at beat 32);
   - an automation lane with curve and step segments, with steps on on-frame beats;
   - a sync LFO (1/16 sine) and a free LFO;
   - a clip-edge step envelope;
   - clips starting on off-grid beats and on on-frame beats at 150 BPM;
   - a session launch quantised to 4 beats.
2. Run A: Play at instant 0, 900 ticks one step apart.
3. Run B, seeded:
   - Play pressed at a random phase within a frame;
   - each tick's instant jittered by ±40% of a frame;
   - one 3-frame stall.
4. Record per transport index: `time` bits, `beat` bits, active clip ids, every param `value` bit, session slot state.
5. For every index both runs rendered, the records are equal bit for bit. Today this fails at the first tick, because the press phase moves every beat.

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
- `jump_over_two_frames_launches_and_resets` (D6)

**Export regression:** the export tests above, plus the existing ones untouched: the `param_step_clip_edge.rs` export arm, `crates/manifold-app/src/rt_dynamic_export_tests.rs`, and the scoped GPU proofs `scripts/gpu_scope.py` maps for `content_export.rs`.

## 6. Phasing

P1 and P2 land together as one merge. That merge keeps today's external-sync behaviour as a named stopgap. Peter then rig-tests it. P3 lands next. P4 lands separately. Repeatable video frames are not promised before P4.

### P1 — The clock type, CPU only

- **Entry:** branch `feat/frame-exact-clock` at or after 09a44cb4c; `rg -n 'struct TransportClock|transport_clock' crates` = 0.
- **Read-back:** sections 2 and 3.1; `crates/manifold-physics/src/clock.rs:278-400`; `driver.rs:167-200`. Restate D1, D4, D6, D8 and D14 and the forbidden moves.
- **Deliverables:**
  - `crates/manifold-playback/src/transport_clock.rs` with the section 3.1 API, private `PhaseLock` and `LiveTempo`;
  - `crates/manifold-playback/src/frame_grid.rs`, the D2 deadline grid that P2's `FrameTimer` wraps;
  - `pub mod transport_clock;` and `pub mod frame_grid;` in `crates/manifold-playback/src/lib.rs`;
  - the `transport_clock` tests named in section 4.
- **Gate:** `CARGO_BUILD_JOBS=4 cargo nextest run -p manifold-playback transport_clock`; `cargo clippy -p manifold-playback -- -D warnings`. Negative: `rg -n 'Instant|SystemTime' crates/manifold-playback/src/transport_clock.rs` = 0.
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
  - `set_beat` + `sync_time_from_beat` become `follow_external_beat`, which puts the index on the frame nearest `map(b)`.
  - Nudges become `locate_nearest`.
  - So under CLK, timecode, AbletonOSC and Link, MANIFOLD follows the external source as it does today, rounded to the nearest frame (at most half a frame from today's value). Being exactly today's behaviour would need an off-grid time source next to the clock, and that would re-open the path this bead closes.
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

### P3 — External sources correct the grid

- **Entry:** P1+P2 landed and Peter's rig test passed. `rg -n 'STOPGAP\(frame-exact P3\)|external_time_sync|follow_external_beat' crates` lists the work.
- **Read-back:** audit items 1.4 and 1.6, D6–D8, D11, D15, section 3.5; `midi_clock_sync.rs:120-160`, `:520-806`; `link_sync.rs` whole; `osc_sync.rs:355-430`; `transport_sync.rs`; `content_thread.rs:1694-1987`, `:2030-2113`.
- **Deliverables:**
  - MIDI tick timestamps (D7).
  - Arrival instants for timecode and AbletonOSC.
  - The M4L timecode sender sends unquantised song seconds (D7); its test sweeps the send period and start phase.
  - Projection to the deadline for each source.
  - `observe_external` wired for all four sources.
  - `LiveTempo` and the single converter under CLK/Link (D8).
  - Authority-change clearing.
  - The Link offset at the anchor deadline with quantum rounding (D15).
  - `external_time_sync`, `follow_external_beat` and every stopgap marker deleted, including from the test mocks (`crates/manifold-playback/tests/osc_timecode.rs:73-88`, `midi_clock_sync.rs:1011-1024`, `osc_sync.rs:624-639`, `sync.rs:250-261`).
  - One info log per correction, e.g. `[Transport] external correction +1 frame (CLK drift 13.1 ms)`.
  - The P3 tests in section 4, plus a CLK, a Link and a Link-initiated-play scenario in `tests/sync_engine_integration.rs`.
- **Gate:** `cargo nextest run -p manifold-playback`; `cargo nextest run -p manifold-app content_thread`; clippy. Negative: `rg -n 'external_time_sync|follow_external_beat|STOPGAP\(frame-exact P3\)|fn sync_time_from_beat|fn set_beat' crates` = 0.
- **Demo:** L4 on Peter's rig.
  - Ableton sends MIDI clock for 5 minutes, including a tempo ramp. The log shows no more than one correction per ramp, and LFO motion is smooth where it stepped before.
  - Link: start from Ableton after a Stop. MANIFOLD follows position, and its bar lines up with Ableton's.
  - The click-script goes in the landing merge commit.
- **Forbidden:** filtering inside the controllers (the lock owns it); re-tuning the 2 s and 0.5 s seek thresholds; a beat in `TransportClock`.
- **Size:** about 450 lines plus 350 of tests.

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
- Run `scripts/gen_docs_index.py`.

Total estimate: about 1,450 lines changed plus 1,300 lines of tests across P1–P4. The first draft's 1,100 plus 950 grew with:
- timestamped sources and per-source projection;
- the live-tempo segment and the single converter;
- the persistence rule;
- the fail-first app test;
- the fps self-sync and the export ordering fix.

## 7. What may look or feel different on stage

- **Repeatable under MANIFOLD's own clock after P1+P2.** Automation, LFOs, clip cuts and launches land on the same frame each time. Video and water are not yet covered (D16).
- **On-frame cuts move one frame earlier where they were late.** At 150 BPM on a 60 fps project, about 40% of on-beat cuts showed one frame late. They now land on their frame, in live and in export.
- **Play and resume don't repeat a frame.** A seek while playing shows the frame you seek to, then moves on.
- **Seeks land up to one frame after the click.** Invisible at normal zoom.
- **Under external sync, P1+P2 behave as today,** rounded to the nearest frame. Still stair-stepped under MIDI clock until P3.
- **After P3, MIDI clock gets smoother.** Every frame advances one frame, and the beat follows Ableton's tempo, ramps included. Visuals sit up to a tick earlier than today, because today's value lags the true clock by up to one tick.
- **Corrections show as one held or skipped frame.** With projected sources and live tempo they should be rare. The log names each one.
- **Link starts on the bar after P3.** A Link-initiated play may move MANIFOLD's start by up to half a bar so its bar lines up with Ableton's.
- **After a hitch, two content frames go out close together.** Presentation keeps three drawables, so this should not show. The P2 gate measures it.
- **Long stalls jump**, as today. Water slows down rather than jumping, as today, and is not repeatable under load.
- **Exports started while Ableton drives no longer freeze.**
- **A 120 Hz display, MIDI-in latency and paused visuals are unchanged.**

## 8. Decided — do not reopen

1. Integer frame index; time = index × (1 / fps as f64) from song time 0, live and export alike.
2. The content timer uses anchored grid deadlines counted by instant comparison; the UI timer is unchanged.
3. Stalls skip and stay in time; no cap.
4. Seeks snap forward (`ceil` with 1e-7 tolerance). Only a seek while playing holds. Play and resume advance normally. External locates go to the nearest frame without a hold.
5. One tick dt (`frames × step`) for every render consumer, in every transport state.
6. External sources correct, never drive. Sources keep smoothed noise under ¼ frame. Act above ½ + ¼ frame on 6 consecutive observations on one side; subtract applied corrections; seed at each anchor; clear on authority change. Up to 2½ frames: one frame per correction, never backward. Beyond: a jump. Coarse locates re-seed on two agreeing samples. Beyond the seek threshold: a seek.
7. Sources are projected to the frame deadline from timestamped arrivals, with per-source bias removed.
8. Under CLK or Link the beat follows Ableton's live tempo through a segment anchored at the last correction; time stays on the grid; the map is the truth under Internal. Peter, 2026-10-13.
9. On a Link-initiated play the offset rounds to Link's quantum. Peter, 2026-10-13.
10. Export shares the clock, set before the first seek; live equals export, not bit-identity with old export.
11. `project.settings.frame_rate` is the only fps; the engine and the timer follow it.
12. A frame's beat is taken at `time + 1e-7 × step`.
13. Physics and liquid stay outside the determinism claim.
14. P1+P2 land first with a named external-sync stopgap; P3 after Peter's rig test; P4 separate.
15. Export runs on the song-time-0 grid: frame k is the first export-grid frame at or after the start, plus k. Peter, Q2.

## 9. Deferred

| Item | Revive when |
|---|---|
| `frame_count` seeds in effects are tick counts since launch; generators always get 0 (`generator_renderer.rs:922`) | A particle look must repeat exactly across runs |
| Physics and liquid determinism under load (`clock.rs:125-130`, `:380`) | Peter needs the water to repeat across performances |
| Bit-equal integrator state across different drop patterns | A dt-integrating look visibly differs between two plays |
| Stale live tempo written into the map after Link peers vanish (`content_thread.rs:2083-2103`) | Any beat jump report after Link drops; file a bug bead at landing |
| `SetFrameRate` writes the setting outside `EditingService` (`content_commands.rs:1653-1657`) | The next settings-mutation audit |
| MIDI launch quantise inert (CORE_ENGINE_MAP.md section 13 (Honest edges), item 2) | Its own bead |
| Audio and video share mach time in Internal mode; the audio device clock may drift ±50 ppm (about 0.36 s per 2 hours) | Audio voices reseek mid-set (200 ms threshold) |
| Live tempo for the AbletonOSC relay (D8) | The relay gains a tempo channel and Peter wants it to drive tempo |

## 10. Open questions for Peter

None open. Q1 is D8, Q2 is D17, Q3 is D15.
