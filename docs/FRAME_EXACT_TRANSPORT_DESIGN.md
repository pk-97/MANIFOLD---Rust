# Frame-Exact Transport — one frame grid for live play and export

**Status:** IN PROGRESS · 2026-10-14 · P1 and P2 landed on main (ff922660e). P3 is redesigned for Peter's two upgrades of 2026-10-14: a tempo-trim clock follower and output latency compensation. Owed: adversarial review of this revision, P3 on `feat/frame-exact-sync`, Peter's rig test, P4, and open questions Q4 and Q5 (section 10 (Open questions for Peter)). Bead: BUG-gqhep (live-transport-nondeterministic-frame-clock).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

<!-- index: Frame-exact transport clock (BUG-gqhep): impact map of every transport time/beat reader and writer, dt consumer, integrator, external sync source, export and harness; the integer frame-index clock shared with export on the song-time-0 grid, the content-tick grid, on-frame edge sampling, external sync as a tempo-trim clock follower (PI loop, trim cap, snap rule) over timestamped sources read at the screen instant with a per-rig output latency, Ableton live-tempo following, phases P1–P4 and the fail-first proof plan. -->

Live play today moves the transport by measured wall time. Each frame samples whatever beat the jittered sum reached, so two plays of the same arrangement give different values on the same frame. Export does not have this problem: it computes time from a frame number. This design gives live play the same kind of clock. Time becomes a function of an integer frame index. Wall time only decides how many frames to step.

Peter's decision, 2026-10-13, quoted from the bead: "One frame-exact transport clock shared with export. Frame grid anchored at song time 0: frame_index N ↔ time N/project_fps; all consumers sample time(frame_index) computed absolutely, never accumulated; real dt only decides how many frames to advance (normally 1). Dropped frame → skip the index (stay in time). Link/MIDI clock/timecode corrections snap to whole frames (nudge the grid, never off-grid time); a correction shows as one held/skipped frame only when drift exceeds half a frame. Seek re-anchors to the grid; loops wrap the index; grid uses project fps, not display rate. Out of scope: audio-triggered clips (depend on live input)."

Peter's upgrades to P3, 2026-10-14, relayed by the lead. First, external sources correct phase with a small, slow, capped tempo trim, the standard clock follower, instead of held or skipped frames. Second, every external source is read at the moment the frame reaches the screen, using a fixed output latency set once per rig. They replace the "held/skipped frame" part of the 2026-10-13 ruling. D6, D7, D8 and D19–D22 carry them.

**For the instrument:** after P1+P2, an LFO, an automation lane or a clip cut lands on the same frame every time the same section plays under MANIFOLD's own clock. After P3, while Ableton drives, MANIFOLD's beat stays within about a millisecond of Ableton's (Link and MIDI clock) and moves smoothly: no held or skipped frames, no MIDI clock stair-steps, and a tempo change in Ableton is followed at once. Frames then repeat only as closely as Ableton's own timing does. Video frames and the water simulation are not covered until P4 and never, respectively (D16).

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
| The stopgap shows the source one frame ahead: it compares `target + step` with the frame after the advance | `engine.rs:1182-1188` | Pre-P2 place-then-advance did the same. It hides about one frame of pipeline delay. D19 and Q4. |
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
| The backward-move guard is beat-based | `engine.rs:1815-1824` | A backward snap resets trigger delivery, as backward nudges do today (D6). |
| No output-latency setting exists | `rg -i 'output.?latency|display.?latency|av.?offset' crates` has no hits | D19. |
| Machine preferences live in `UserPrefs`, a JSON key-value file outside any project | `user_prefs.rs:1-16`, `:62-164`; keys like `MANIFOLD_BlenderPath` (`blender_import.rs:32`) | D19 stores the latency there. |
| The footer FPS field opens the shared text-input overlay | `app_render.rs:1401-1415`; `TransportAction::FpsFieldClicked` (`panels/actions.rs:102`) | D19 copies the pattern; no new widget. |
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
- Under external authority (P3), a source's seek goes to the frame nearest the target's map time, with no hold, and the follower re-seeds there at the source's beat (D6). A hold would start every external locate half a frame behind on average.
Why "at or after": a clip spans `[start, end)`. The first frame that shows a clip starting at `s` is `ceil(s / step)`.
Rejected: nearest rounding for user seeks, because a seek to a clip start could land just before it and show black.

**D5 — One tick dt for everything that renders.** `tick_dt = frames × step`, in every transport state. It feeds generators, effects, the compositor, modulation's audio followers, audio triggers and video `pre_render`. Export frame 0 has `frames = 0`, so `tick_dt = 0` as today.
Rejected: a zero dt while paused. Paused graphs would freeze.

**D6 — External sources steer the beat with a slow, capped tempo trim. Time never leaves the grid.** Peter, 2026-10-14. This replaces the 2026-10-13 held/skipped-frame rule and P1's phase lock.
- Under an external authority (Link, CLK, AbletonOSC, timecode), each tick advances the index by exactly `frames`. The index never holds, skips or jumps for phase. Time stays `index × step` (D1).
- The beat comes from the **follow segment**: `beat(t) = anchor_beat + (t − anchor_time) × rate`, with `rate = base_bpm / 60 × (1 + trim)`. `base_bpm` is the source's tempo (D21). `trim` is the follower's correction. D8 makes the segment the engine's one converter under an external authority.
- **The loop.** One sample per tick, taken after the advance. `t` is the grid time of the frame this tick shows. `Δt` is the grid time since the last sample (`frames × step`). `b_src` is the source's beat at the frame's screen instant (D7, D19). `b = beat(t)` before the update.
  - Phase error in seconds: `e = (b_src − b) × 60 / base_bpm`.
  - Pre-filter: `e_f += (1 − exp(−Δt / τ)) × (e − e_f)`.
  - Integral: `I = clamp(I + K_i × e_f × Δt, −C, C)`.
  - Trim: `trim = clamp(K_p × e_f + I, −C, C)`.
  - Re-anchor the segment at `t` with the new rate. The beat at `t` does not change, so frame N keeps the beat it had before the sample; the new rate runs from N to N + 1.
  - `K_p = 2ζω_n`, `K_i = ω_n²`. This is the second-order loop of the delay-locked loop JACK uses (F. Adriaensen, "Using a DLL to filter time", 2005), with one change. The classic DLL also moves the phase by `b × e` on every sample, which would make the beat jump a little each frame. Here every correction goes through the rate, so the beat stays continuous. With ζ = 1 an error dies away without ringing. `Δt = 0` (a tick with nothing advanced) updates nothing.
- **Constants.** Two profiles, one per source class:

| | Fast profile: Link, CLK | Slow profile: AbletonOSC, timecode |
|---|---|---|
| ω_n, ζ | 1.0 rad/s, 1 | 0.2 rad/s, 1 |
| K_p, K_i | 2.0 /s, 1.0 /s² | 0.4 /s, 0.04 /s² |
| Pre-filter τ | 0.1 s | 0.5 s |
| Trim cap C | ±0.5% | ±0.5% |
| Snap threshold | 25 ms | 60 ms |
| Snap confirmation | 3 fresh samples, all within 10 ms of each other | 2 fresh samples within 30 ms |
| Follows errors that last | about 1 s or more (closed-loop corner 0.4 Hz) | about 5 s or more (corner 0.08 Hz) |

- **Why these numbers.** They come from a scratchpad simulation of this loop against the D7 noise models: 60 fps, seeded jitter, one-hour runs for the steady cases. P3's tests re-measure them in Rust (section 5 (Proof plan), P3 proofs).
  - ω_n = 1 rad/s for Link and CLK. It removes a tempo change's leftover error (under 2 ms with Link tempo) in about a second. It keeps ±1 ms of tick jitter out of the beat speed (under 0.1%). ω_n = 2 doubled the speed noise and gained nothing in phase; ω_n = 0.5 settled 60% slower.
  - ω_n = 0.2 rad/s for AbletonOSC and timecode. Their noise is about ten times larger: 10 Hz reports with Live-side skew, and 25 fps quantising. At ω_n = 1 the trim swung against its cap all the time; at 0.2 the AbletonOSC phase holds within 3.3 ms.
  - The pre-filter keeps single late ticks and frame-to-frame noise out of the proportional term, which would otherwise pass them straight to the beat speed.
  - Cap ±0.5%. The trim must hold the steady rate gap between Ableton's audio clock and the Mac's clock (about ±100 ppm, 0.01%) plus the tempo estimate's error (under 0.05%), with room left to remove phase error at 5 ms per second. 0.5% is 0.6 BPM at 120; no LFO, cut or video rate shows it. A 0.2% cap settles a 10 ms step about as fast (4.6 s against 4.2 s), but a 20 ms error then takes 10 s. The integral is clamped to the same cap, so a long saturation cannot wind up.
  - Snap at 25 ms. That is above the fast profile's worst normal transient: a tempo rise under CLK alone peaks at 15 ms, and with Link tempo every tempo jump stays under 1.6 ms (D20). Below it the trim removes the error within about 6 s. Above it, the source has moved, not drifted. The slow profile's 60 ms sits above its sources' noise (timecode quantising alone reaches 20 ms).
- **Settle time** (simulation). Fast profile: a phase error of 5 ms is under 1 ms after 0.8 s, 10 ms after 4.2 s, 20 ms after 6.6 s. Above about 5 ms the trim sits at its cap and removes 5 ms per second, so settle time is roughly `error / 5 ms` seconds plus 2. Slow profile: 10 ms is under 4 ms after 1.9 s, 20 ms after 12 s.
- **Snap.** A sample whose raw error passes the snap threshold does not enter the loop. When the confirmation count of fresh samples have all passed it and agree, the segment re-anchors at `(t, b_src)`: the beat jumps to the source on that frame. The pre-filter clears and `trim = I`. The integral is kept, because the rate gap it learned is still true after a relocation. A lone outlier changes nothing.
  - "Fresh" means the sample carries new source data: every Link read; a CLK read after at least one new tick; an AbletonOSC or timecode read after a new message. A projection of an old message never confirms a snap.
  - A snap moves the beat, not the index. Everything that reads the beat sees the jump. Everything that reads time sees none: the temporal reset rule (audit item 1.3) and the physics clock carry on, so trails, feedback and water continue through a small relocation.
  - A backward snap trips the beat-based backward-move guard (`engine.rs:1815-1824`) and resets trigger delivery, as backward nudges do today. A snap never bumps `transport_epoch` and runs no seek side effects.
- **Seek.** At or above the source's seek threshold the existing seek path runs, thresholds unchanged: MIDI clock 2.0 s plus the tick-delta range check (`midi_clock_sync.rs:768-772`, `:783`), timecode 0.5 s (`osc_sync.rs:421`), AbletonOSC 2.0 s (`transport_sync.rs:31`). Sources now pass the target as a beat. The engine bumps the epoch, runs the seek side effects, moves the index to the frame nearest the beat's map time with no hold (D4), and seeds the follower there at the source's beat.
- **Seed.** The first sample after a seed point sets the segment to `(t, b_src, base_bpm)` with `trim = 0`, `I = 0` and the pre-filter cleared. Seed points: Play or resume under an external authority, every external seek, and a tempo-map edit under timecode (D11). Until the first fresh sample after a seed point, the segment free-wheels from where it was. The seed then moves the beat to the source's position once: under CLK on the first tick, at most one tick period after Play; under Link on the same tick. That move belongs to starting or seeking, like today's Play alignment to MIDI clock (`content_commands.rs:894-911`).
- **Handover from Internal while playing** is not a seed. The segment starts at the current frame's own beat and the source's tempo, so nothing moves on that frame. The source's first samples then trim, snap or seek like any others: Ableton's position decides, through the normal rules.
- **Free-wheel.** A tick with no sample (gated, stale or not yet arrived) leaves the segment alone, so the beat runs on at its last rate. Today's gates stay: the seek cooldown and the pending AbletonOSC ack for CLK (`content_thread.rs:1997-2003`), and MANIFOLD owning playback for Link (`:1977`). A CLK source goes stale after four tick periods with no tick (D20), so a clock gap free-wheels instead of extrapolating.
- **Handover between external sources** is bumpless, the standard control term for switching controllers without a kick: the segment is kept, `I = trim`, the pre-filter and the snap count clear, and the new source's profile applies. If the new source disagrees by more than its snap threshold, its first confirmed samples snap.
- **What the follower guarantees while playing.** The beat is continuous; only snaps and seeks jump it. The beat strictly increases, because `rate ≥ 20 / 60 × 0.995 > 0`. Beat speed is within the cap of `base_bpm`. Per-source phase and speed bounds are in D7.
- **What is left of whole-frame correction.** Nothing. `Correction` (Hold, Skip, Jump), `PhaseLock`, `Smoothing`, `observe_external`, `clear_external`, `last_correction`, and the stopgap's `follow_external`, `defer_external` and `take_pending_external` are deleted, not kept beside the follower. Two of `PhaseLock`'s ideas carry into the new code: the first sample after a clear seeds the estimate, and a far-off sample needs agreeing company before it moves anything. Under an external authority the only index placements are seeks (D4) and the hand-back remap (D8).
- Rejected:
  - Held or skipped frames for phase (the 2026-10-13 rule). Each is a visible stutter, and the persistence it needs adds 100 ms of lag. Peter replaced it.
  - Moving the phase directly on each sample, as the classic DLL does. The beat would jump a little every frame.
  - No cap. Source noise would reach the beat speed, and a large error would visibly speed up the show.
  - Trimming time instead of the beat. Time is the grid (D1).
  - Copying Link's beat straight onto each frame. It is exact, but the beat would follow every small step of Link's network clock (an assumed size, see D7), and one loop for all four sources is simpler to prove.

**D7 — Each source is read at the frame's screen instant and projected from timestamped arrivals.** The read instant is `R = deadline + L`: the deadline of the frame this tick shows (`FrameTimer::tick_deadline`, `frame_timer.rs:438-442`) plus the output latency (D19). Each source hands the engine one sample per tick: `ExternalSample { beat, bpm, fresh }`, the source's beat at `R`, its base tempo (D21) and whether it carries new data (D6).
- **Link.** `beat_at_time(R)` minus the cached offset (D15), with `R` mapped into Link's clock as today (`link_micros_at`, `link_sync.rs:184-193`). Exact at any instant; fresh every tick. Noise model: no jitter; the session tempo runs up to ±100 ppm off the Mac's clock; Link's clock sync shifts the timeline by up to ±0.2 ms every 30 s. ⚠ VERIFY-AT-IMPL: the last two are assumptions about Link's network-time filter, not read from its source. The loop absorbs any constant rate gap inside the cap and any step under the snap threshold, so the bounds hold for values well beyond them.
- **MIDI clock.** The D20 estimator gives position and period at `R`: `beat = (k0 + (R − t0) / period) / 24`, where `t0` is the filtered arrival of tick `k0`. Fresh after a new tick. Noise model: arrival jitter of ±1 ms uniform plus 0.3 ms Gaussian, 0.5% of ticks 4 ms late, and an 80 ppm rate gap.
- **AbletonOSC.** The receive thread stamps an `Instant` on each packet (today the frame's drain time is used, audit item 1.10). `beat = reported beat + (R − arrival + RTT/2) × bpm / 60`, with the relay's own round-trip estimate (`transport_sync.rs:32-37`) and Live's tempo from the tempo listener (D8). The latest-only slot (`ableton_bridge.rs:907-910`) keeps the latest report with its stamp. Fresh after a new song-time report. Noise model: reports at 10 Hz ± 20 ms, each up to ±10 ms off Live's audio position, ±1 ms network jitter. ⚠ VERIFY-AT-IMPL on the rig: the Live-side skew is a guess; P3's rig log measures it.
- **Timecode.** The receive thread stamps each packet. Song time at `R` is `s = seconds + (R − arrival)`. Then `beat = map(s)` and `bpm = 60 × (map(s + step) − map(s)) / step`: the map's tempo over the next frame, so a tempo point in the map is followed exactly. One-float song seconds keep `f64` when sent as an OSC double. H:M:S:F gains half a timecode frame: a frame label means the frame has started, so on average the true time is half a frame later. Fresh after a new message. Noise model: LiveMTC labels 25 fps frames, sent on a free-running metro; arrival jitter up to 1 ms.
  - The sender is LiveMTC, not ours (audit item 1.10). P3 changes only the receiver. A MANIFOLD-owned sender of unquantised song seconds is deferred; it would bring timecode down to the "unquantised" row below.
- **Bounds.** Simulated worst case at steady tempo, and the bound P3's tests assert (about twice the simulated worst). "Speed" is the beat speed against the source's true tempo; "per frame" is how much that speed may change from one frame to the next.

| Source | Simulated worst (phase; speed; per frame) | Test bound: phase | Test bound: speed | Test bound: per frame |
|---|---|---|---|---|
| Link | 0.2 ms; 0.033%; 0.006% (1 h; phase against Link's own timeline, which Live also plays on) | 0.5 ms | 0.1% | 0.01% |
| CLK with Link tempo | 0.42 ms; 0.08%; 0.007% (1 h) | 1 ms | 0.2% | 0.02% |
| CLK alone | 0.51 ms; 0.11%; 0.015% (1 h) | 1 ms | 0.25% | 0.03% |
| AbletonOSC | 3.3 ms; 0.31%; 0.022% (1 h) | 8 ms | 0.6% | 0.05% |
| Timecode, unquantised seconds | 0.62 ms; 0.01%; 0.001% (5 min, six metros) | 2 ms | 0.1% | 0.01% |
| Timecode, LiveMTC 25 fps labels | 15.4 ms; 0.5%; 0.05% (10 min, six metros) | 21 ms | 0.6% | 0.1% |

- What the bounds mean on stage. At 60 fps a frame is 16.7 ms. Under Link and CLK the beat is within a millisecond of Ableton's, under a tenth of a frame. A cut lands on another frame than a perfect clock would pick only when Ableton's beat falls within that millisecond of a frame boundary, and then by one frame, which is the choice a perfect clock would make a millisecond later. AbletonOSC is the degraded fallback tier and stays within half a frame. LiveMTC timecode is limited by its own frame labels.
- Sources do no other filtering. The follower owns phase filtering; the D20 estimator is the one exception, because tick timing must be turned into a position and a tempo first.

**D8 — While Ableton drives, the beat follows Ableton's tempo through the follow segment. Under Internal, the map is the truth.** Peter's ruling, 2026-10-13, extended to all four external sources on 2026-10-14.
- Time is always the grid.
- Under Internal authority: `beat(N) = map(time(N))`.
- Under Link, CLK, AbletonOSC or timecode authority, the beat comes from the follow segment of D6, computed from the index each frame, never accumulated. Its base tempo is Ableton's: Link's session tempo, the CLK estimate, the AbletonOSC tempo listener (`transport_sync.rs:326-331`), or for timecode the map's tempo at the source's song time (D7, D21).
  - AbletonOSC was on the map until now because its relay was thought to have no tempo channel. It has one. Following it keeps the beat continuous when CLK dies mid-set and AbletonOSC takes over: both then follow Live's tempo, so the handover needs no snap.
- The anchor is a time, not a frame index. An fps change (D10) or the export grid (D9) remaps indices, and an index anchor would then sit at a different time and jump the beat: anchored at frame 600 at 120 BPM with the playhead at 20 s, `set_fps(30)` read beat 20 instead of 40. The anchor time is always a grid time, so the beat is still a function of the index between samples.
- The segment re-anchors at the shown frame on every sample (D6), keeping the beat continuous. An Ableton tempo ramp is a run of one-frame segments.
- Every beat↔time conversion in the engine goes through one converter that is either the map or the segment. That covers the 13 `TempoMapConverter::` calls and the helpers at `engine.rs:2133-2170` and `:3266` (pre-P2 anchors; re-run the audit item 1.2 command). Clip edges, video in-points and session targets therefore agree with the beat on screen.
- When authority returns to Internal, the beat is kept and the index remaps to the first frame at or after `map⁻¹(beat)`, as a tempo edit does (D11). The position never jumps on hand-back. From then on the beat runs at the map's tempo.
- Export always uses the map (D9). An export started while Ableton drives renders the arrangement, not the live follow.

**D9 — Export shares the clock, set before anything moves.**
- `begin_export_clock(fps)` runs before the first seek (`content_export.rs:717`). The two seeks, the warm-up ticks and the frame loop all run on the export grid at export fps.
- Export frame k sets the index to `index_at_or_after(start) + k` on the song-time-0 grid at export fps (D17), so live and export sample identical times.
- In export mode the tick sets the index regardless of external sync, so an export under CLK or timecode advances (audit item 1.1).
- Bit-identity with old export is not a goal. Live equals export is the requirement. D14 changes on-frame edge results in export as well as live.
- The end of export restores the live rate and seeks to the restore time as today (`content_export.rs:427`, `:497`).

**D10 — One source of truth for fps.** `project.settings.frame_rate` is the only fps. Each tick, outside export, the engine compares its clock fps with it and remaps when they differ. It keeps the current time and moves the index to the first frame at or after it, with no hold. The content thread then retargets the timer from `engine.frame_rate()` when the two differ. The four `timer.set_target_fps` copies in `content_commands.rs` are deleted. The rt-capture override becomes a project setting written before load.
The clock and the tick grid accept 1 to 1000 fps through one rule (`sanitize_fps` in `transport_clock.rs`). Anything else, including NaN and infinity, runs at 60 with a one-time warning naming the bad value, so a corrupt setting never stops the transport.

**D11 — Tempo edits during play keep the musical position.** Under Internal authority, after a map or BPM edit, the beat is kept and the index remaps to the first frame at or after `map'(beat)`, with no hold. Under Link, CLK or AbletonOSC the segment governs the beat, so a map edit changes nothing on screen until hand-back (D8). Under OSC timecode, song time is the master and the source's beat is `map(seconds)`, so a map edit is a seed point (D6): the next sample re-seeds the segment, and the beat moves at once to the new map's beat for the same song time. The `external_time_sync` early return in `reconcile_tempo_edit` (`engine.rs:807-810`) goes with the flag.

**D12 — Video picks its frame from transport time.** Each tick, a playing clip's target source frame is `floor(video_time × video_fps)` from `compute_video_time` (`engine.rs:2559`). It requests that frame when it differs from the one shown and seeks when the gap exceeds the existing 3-frame rule. Both integrators go: `time_accumulator` and `playback_time += dt × rate` (`video_renderer.rs:812-813`). Decode stays asynchronous: a late decode holds the previous frame. P4 owns this.

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

**D16 — What the determinism claim covers.** Bit-equal per frame index on two runs, whatever the press phase, jitter or stalls: time, beat, clip membership, automation, LFOs and drivers, session launches, one-shot expiry, and every DERIVED reader in audit item 1.2. Integrators fed tick dt are bit-equal only when the two runs dropped the same frames (audit item 1.3). Video frames: not until P4. Physics and liquid: never. The sim's live cap and reanchor read wall load (`clock.rs:125-130`, `:380`), so water under load differs between runs. That is LIVE_SIM_CLOCK_DESIGN's choice of slow motion over stutter, and this design does not reopen it.
Under an external authority the claim is different, because the beat follows Ableton's timing. Time per index is still exact. The beat is continuous, strictly increasing between snaps and seeks, and within D7's bounds of the source. Two plays of the same section match each other only as closely as Ableton's own timing and the arrival jitter allow.

**D17 — Export runs on the song-time-0 grid.** Peter's ruling on Q2, relayed 2026-10-14. Export frame k is the first export-grid frame at or after the export start, plus k. Live and export then sample identical times for the same song moment. An export started off the grid begins up to one frame after its marker: at 128 BPM and 60 fps, half a frame on every odd bar.
Rejected: frame 0 exactly on the marker. Export would then sample times live play never renders. Putting both on the marker would mean anchoring the live grid wherever Play was pressed, which is the non-determinism this bead removes.
Consequence: the clock has no origin (D1). `begin_export(fps)` and `end_export(fps)` change the step and keep the current time.

**D18 — Bad input never moves or breaks the clock.** `locate`, `seek_playing` and `locate_nearest` ignore non-finite times. The follower ignores a sample whose beat or tempo is not finite, and clamps tempo to 20–300 BPM as the map does. The output latency accepts 0–250 ms (D19). Absurd finite times clamp the index to ±2^52, where `index × step` is still exact. A phase error past the snap threshold never enters the loop, so an absurd sample can at most snap after its confirmations, and every source seeks long before that. Index sums saturate, and the tick grid's deadline indices stop at 2^32, so no arithmetic overflows and no `Duration` conversion sees a bad value.

**D19 — Output latency: every external source is read for the moment the frame reaches the screen.** Peter, 2026-10-14.
- The read instant is `R = deadline + L` (D7). `L` is the time from a frame's deadline to light on the screen or projector: MANIFOLD's own render and presentation, plus the display's processing. It applies to all four external sources. Internal authority never reads it. This is Link's guidance for a peer with output latency: ask for the beat at the moment the output is perceived, not when it is computed.
- One number per rig, a machine preference: `UserPrefs` key `MANIFOLD_OutputLatencyMs` (`user_prefs.rs:62-164`), next to keys like `MANIFOLD_BlenderPath`. Not a project setting: a project moves between rigs, and the delay belongs to the rig. Never measured per frame: a per-frame measurement would put presentation jitter into the beat.
- Default 0, per Peter. The accepted range is 0–250 ms; anything else, non-finite included, loads as 0 with a one-time warning naming the value. No negative values: Live already shifts what it publishes over Link for its own output latency, and Live's MIDI clock output has its own per-port sync delay.
- UI: a footer field beside the FPS field, reading "Out 0 ms". A click opens the shared text input, as the FPS field does (`app_render.rs:1401-1415`). On commit the UI thread stores and saves the pref and sends `ContentCommand::SetOutputLatency(Seconds)`. At startup the app sends the stored value before the content thread's first tick. It is not a project edit, so it has no undo and does not go through `EditingService`, like every other pref. It is one field, not a manifest-backed param surface, so WIDGET_TREE_DESIGN.md section 5b (Agent contract & enforcement) does not apply.
- Setting it on the rig: a MANIFOLD clip that flashes on every beat, a click in Live on every beat, a phone filming screen and speaker at 240 fps. Raise `L` by however late the flash is.
- **One frame against today (Q4).** The P2 stopgap and the pre-P2 engine both show the source one frame ahead (audit item 1.10). That hides about one frame of pipeline delay by accident. With `L = 0`, P3 shows the source's beat at the deadline itself, so visuals sit later than today until `L` is set: one frame (17 ms at 60 fps) under Link, about 10 ms under MIDI clock, where today's stair-step lag and the early tick count (D20) partly cancel. `L` = 17 ms restores today's Link alignment.
- Rejected: a project setting; measuring presentation each frame; compensating Link only.

**D20 — MIDI clock: timestamped events, MIDI 1.0 position rules, and an adaptive tick estimator.**
- **Events.** The midir callback sends every clock message with its arrival time through a `std::sync::mpsc` channel, the note input's shape (`midi_input.rs:115`, `:470`): `ClockEvent { kind: Tick | Start | Continue | Stop | SongPosition(u16), at: Instant }`. The packed `AtomicU64` state and its CAS loop (`midi_clock_sync.rs:19-78`) are deleted. The content thread rebuilds the position from the events, in order, each tick. That removes the 14-spare-bits problem and the two-word read race the earlier plan had to work around.
  - ⚠ VERIFY-AT-IMPL: midir's timestamp base on macOS. If it is CoreMIDI host time in microseconds, map it to `Instant` once with a paired read of both clocks. If not, stamp `Instant::now()` in the callback; CoreMIDI callback latency is well under a millisecond.
- **Position rules (MIDI 1.0).** Start arms position 0. Continue arms the current position. Song Position Pointer sets the position in sixteenths. The first Tick after Start or Continue lands on the armed position; each later Tick adds 1/24 beat. Ticks while stopped never move the position. Today every Tick counts, so the beat runs one tick early (audit item 1.10). ⚠ VERIFY-AT-IMPL with a raw byte log from Live: whether Live sends ticks while stopped, and whether it sends Song Position Pointer on a loop wrap.
- **Estimator.** A second-order DLL on tick arrival times, in Adriaensen's form: `e = arrival − predicted`; `t0 = predicted`; `predicted += b·e + period`; `period += c·e`; with `ω = 2π·B·period`, `b = √2·ω`, `c = ω²`.
  - `B = 0.25 Hz` in steady state, with each tick's error clamped to ±2 ms, so one late tick barely moves it.
  - Three ticks in a row more than 2 ms off on the same side switch `B` to 4 Hz, unclamped, until 0.5 s after the last such run. Then it narrows back.
  - It seeds on the first two ticks after Start, Continue or a Song Position Pointer: `t0` is the first arrival, `period` the first interval. Before the second tick the sample uses the D21 tempo or, failing that, the last CLK estimate or the map.
  - Position at `R` is `(k0 + (R − t0) / period) / 24` (D7). Tempo is `60 / (24 × period)`.
  - Stale after four periods with no tick: no sample; the follower free-wheels.
- **Why this estimator** (simulation, 80 ppm, the D7 jitter model): a fixed 1 Hz DLL passed tick jitter to the beat speed as 0.8%, changing by 0.5% between frames. A fixed 0.25 Hz DLL was quiet (0.12%) but slow on tempo changes. Clamping plus a hard re-seed on a run of misses fixed the noise, but ramps re-seeded 11–24 times with peaks up to 21 ms. The adaptive DLL holds 0.11% speed and 0.51 ms phase over an hour, and never widened in steady play. Today's estimator, a 96-tick window with an EMA polled once per frame in `f32` (audit item 1.10), lags about 2 s.
- **Tempo changes under CLK** (fast profile, simulated peak phase error):

| Event | CLK with Link tempo (D21) | CLK alone |
|---|---|---|
| Jump 120 → 140 BPM | 0.7 ms, no snap | 10 ms, no snap, under 1 ms after 3.1 s |
| Jump 128 → 174 | 0.9 ms, no snap | 15 ms, no snap, under 1 ms after 4.0 s |
| Jump 174 → 128 | 1.6 ms, no snap | 28 ms: one backward snap |
| Jump 140 → 70 | 1.6 ms, no snap | 125 ms: two snaps |
| Ramp 120 → 128 over 16 beats | not simulated; Link's tempo is exact at every instant | 3.6 ms, no snap |
| Ramp 120 → 174 over 8 s | not simulated | 4.7 ms, no snap |
| Ramp 174 → 120 over 4 s | not simulated | 5.5 ms, no snap |

  A clock alone cannot know the tempo dropped until the next tick is late, so it runs ahead first and then snaps back (Q5).

**D21 — The base tempo comes from Link when it can. Live tempo lasts one tick.**
- Base tempo per authority:
  - Link: the session tempo.
  - CLK and AbletonOSC: Link's session tempo while Link has peers and agrees within 1% with the authority's own estimate; otherwise the authority's own estimate (D20; the AbletonOSC tempo listener).
  - Timecode: the map's tempo at the source's song time (D7).
- Why Link first. Link knows a tempo change at the instant it happens; MIDI clock reveals it only through later ticks. With Link's tempo, every simulated CLK tempo jump stays under 1.6 ms with no snap (D20). The live tempo already ranks Link first (`content_thread.rs:1819-1827`), and ABLETON_TRANSPORT_SYNC_DESIGN.md names Link the tempo plane of Peter's rig.
- The 1% guard catches a Link session that Live is not part of. Then the authority's own estimate wins, with one log line saying why.
- Live external tempo (the BPM readout, `get_bpm_at_beat`, clip rates) is cleared at the start of every `tick_sync_controllers` and set only by a source that delivered a tempo this tick. It reports `base_bpm`, never the trimmed rate: the trim is a correction, not a tempo.
- `apply_resolved_tempo`'s Internal-authority branch, which writes the live tempo into the beat-0 tempo point (`content_thread.rs:2131-2151`), is deleted. Under Internal the map is the truth (D8). With live tempo lasting one tick, that branch could only write a stale value, or a fresh one on the single frame before authority catches up with new peers. Tempo recording is unchanged. This fixes BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point): when peers vanish, the beat is kept and the project's own tempo resumes (D8).

**D22 — What each source does on each transport event.** "Seed", "snap", "seek", "free-wheel" and "bumpless" are D6's terms.

| Event | Link | MIDI clock | AbletonOSC | Timecode |
|---|---|---|---|---|
| Start | Link-initiated play: arbiter play (`link_sync.rs:153-155`), offset rounded to the quantum, seed (D15). MANIFOLD's own Play: exact offset, seed | Start or Continue: arbiter play as today (`midi_clock_sync.rs:603-611`); seed on the first tick at the armed position (D20) | Relayed Play (`content_thread.rs:1918-1924`); seed on the first fresh report | Timecode arriving means play (`osc_sync.rs:376-382`); seed on the first message |
| Stop or pause | Arbiter pause as today; follower cleared. A MANIFOLD Stop also clears the offset | Stop: arbiter pause; follower cleared | Relayed Pause; follower cleared | 0.5 s of silence means pause (`osc_sync.rs:383-389`); follower cleared |
| Relocate below the seek threshold | Does not happen: Link's beat timeline never relocates, and Live moving its playhead does not move it | Song Position Pointer or a bad tick delta always seeks | 60 ms to 2 s: snap | 60 ms to 0.5 s: snap |
| Relocate at or above the threshold | Not applicable | Seek | Seek (SeekBeats) | Seek |
| Loop wrap in Live | No effect: MANIFOLD keeps Link's beat, as today | A seek if Live sends Song Position Pointer (⚠ VERIFY-AT-IMPL); otherwise the count runs on past the loop end, as today | Song time jumps back: a loop under 2 s snaps, a longer one seeks | Under 0.5 s snaps, longer seeks |
| Tempo change | Base tempo changes at once; leftover phase under 1 ms | Link tempo at once (D21), else the estimator widens (D20) | Tempo listener, one report later; the trim covers the gap | The map's tempo; Live's tempo is not in timecode |
| Tempo ramp | Followed by the base tempo | As above; CLK alone peaks under 6 ms | As above | The map's tempo points, each followed exactly (D7) |
| Source goes quiet | Peers vanish: authority falls; see the hand-back row | Free-wheel; stale after four periods; authority falls after the 0.5 s timeout | Free-wheel until the receive timeout drops OSC authority | Free-wheel, then pause after 0.5 s |
| Handover to another external source | Bumpless (D6); a disagreement past the new snap threshold snaps | Bumpless | Bumpless | Bumpless |
| Hand-back to Internal | Beat kept; index to the first frame at or after `map⁻¹(beat)`; the project's tempo resumes; nothing writes the map (D8, D21) | Same | Same | Same |
| Export | Follower suspended; map beats on the export grid (D8, D9) | Same | Same | Same |

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

Owned by the engine as its converter under an external authority (D8). Pure: no `Instant`, no tempo map, no allocation. `LiveTempo` moves here from `transport_clock.rs` as the follow segment.

```rust
/// D6's constants. `FAST` for Link and CLK, `SLOW` for AbletonOSC and timecode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowProfile {
    pub omega_n: f64,          // rad/s
    pub zeta: f64,
    pub prefilter_tau: f64,    // s
    pub trim_cap: f64,         // fraction: 0.005
    pub snap_seconds: f64,
    pub snap_confirm: u32,     // fresh samples
    pub snap_agree_seconds: f64,
}
impl FollowProfile { pub const FAST: Self; pub const SLOW: Self; }

/// One per tick, from the authority's source at the read instant (D7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalSample { pub beat: f64, pub bpm: f64, pub fresh: bool }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FollowEvent { None, Seeded, Snapped { by: Seconds } }

/// The follow segment (was `LiveTempo`): beat(t) = anchor_beat + (t - anchor_time) * rate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FollowSegment { anchor_time: f64, anchor_beat: f64, beats_per_second: f64 }

#[derive(Debug, Clone)]
pub struct ExternalFollower {
    segment: FollowSegment,
    profile: FollowProfile,
    base_bpm: f64,
    trim: f64,
    integral: f64,
    filtered_error: f64,  // seconds
    last_sample_time: f64,
    seeded: bool,
    snap_count: u32,
    snap_first: f64,      // raw error of the first unconfirmed outlier
}

impl ExternalFollower {
    /// The only constructor: the segment always exists, so the engine's converter
    /// is never undefined. Internal → external handover starts at the frame's own beat (D6).
    pub fn starting_at(profile: FollowProfile, t: Seconds, beat: Beats, bpm: Bpm) -> Self;
    pub fn beat_at(&self, t: Seconds) -> Beats;
    pub fn time_at(&self, beat: Beats) -> Seconds;
    pub fn base_bpm(&self) -> Bpm;
    pub fn trim(&self) -> f64;
    /// Seed point (D6): the segment free-wheels until the next fresh sample sets it.
    pub fn reseed(&mut self);
    /// Seek under external authority: segment at (t, beat), loop state cleared.
    pub fn seed_at(&mut self, t: Seconds, beat: Beats, bpm: Bpm);
    /// Bumpless switch to another source (D6).
    pub fn hand_over(&mut self, profile: FollowProfile);
    /// After this tick's advance; `t` is the shown frame's grid time. Non-finite ignored (D18).
    pub fn observe(&mut self, t: Seconds, sample: ExternalSample) -> FollowEvent;
}
```

Engine side, section 3.2 (PlaybackEngine seam): `tempo: TempoSource { Map, Follow(ExternalFollower) }` is the one converter (D8). `set_external_sample(ExternalSample)` stores this tick's sample; every placement drops it, as the stopgap's pending target was dropped (P2's `placement_after_an_external_target_wins_the_tick` is rewritten against the sample and keeps guarding that). `set_follow_authority(Option<FollowProfile>)` swaps the converter on an authority change: `Some` from Internal builds `starting_at` the current frame, `Some` from another source calls `hand_over`, `None` hands back (D8). `seek_external(beat)` is the arbiter's seek under an external authority (D6).

The tempting wrong turns: holding or skipping a frame "just this once" for a big error (that is a snap or a seek, D6); putting the phase loop inside a source controller (the follower owns phase; only D20's estimator lives in a controller); trimming time instead of the beat (D1).

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
- `derive_external_beat` (`content_thread.rs:1967-2015` on f32e3c5fd): P2 calls `follow_external_beat`. P3 replaces it with `external_sample(authority)`: it computes `R = tick_deadline + L` (D19), asks only the authority's source for its `ExternalSample` at `R` (D7), and hands it to `engine.set_external_sample`. The existing gates stay in front of it (D6, free-wheel).
- `link_sample_at` (`content_thread.rs:589`) becomes `read_at = tick_deadline + L`; Link's `update` and the offset both use it (D15).
- On an authority change, `tick_sync_controllers` calls `engine.set_follow_authority` with the new source's profile, or `None` for Internal (D6, D8). Live tempo is cleared at the top of `tick_sync_controllers` (D21).
- Play aligned to MIDI clock (`content_commands.rs:894-911`): P2 `engine.locate(map(clock beat))`. P3 deletes the locate: the follower seeds on the first tick at the armed position (D20, D22).
- `ContentCommand::SetOutputLatency(Seconds)` stores `L` on the content thread (D19).

### 3.5 External sources (P3)

Anchors on f32e3c5fd.

| Source | Today | After |
|---|---|---|
| MIDI clock | Packed atomic state, no timestamps (`midi_clock_sync.rs:19-78`, `:134`); `set_external_time_sync` (`:586-597`); nudge or seek (`:779-796`); per-frame beat copy (`content_thread.rs:1989-2009`); BPM by windowed EMA (`midi_clock_sync.rs:669-733`) | Timestamped events and MIDI 1.0 position rules (D20). `sample_at(R)` from the adaptive estimator, `FAST` profile (D6, D7). Play and pause unchanged. Seek unchanged (2 s, tick-delta range); the nudge branch and the beat copy are deleted. BPM from the estimator; Link's tempo first when it agrees (D21). |
| Link | Beat at the tick deadline, followed past ¾ frame (`content_thread.rs:1973-1988`, `engine.rs:1182-1188`) | `sample_at(R)`: exact beat minus the offset, `FAST` profile, fresh every tick, while today's gates hold. Offset per D15. |
| OSC timecode | `set_external_time_sync` (`osc_sync.rs:371-373`); nudge or seek (`:405-440`); stamped at drain | Arrival stamped on the receive thread; `f64` seconds; half-frame bias on H:M:S:F; `sample_at(R)` through the map, `SLOW` profile (D7). Seek unchanged (0.5 s); the nudge branch is deleted. |
| AbletonOSC relay | NudgeBeats or SeekBeats actions per report (`transport_sync.rs:302-324`, `content_thread.rs:1933-1954`); stamped at drain (`ableton_bridge.rs:641-662`) | Arrival stamped on the receive thread; `sample_at(R)` with RTT/2 and Live's tempo, `SLOW` profile (D7, D8). SeekBeats unchanged (2 s); NudgeBeats is deleted. |
| Arbiter | `SyncArbiterTarget::nudge_time`, `set_external_time_sync` (`sync.rs:23-27`, `:181-228`) | Both deleted. Samples come only from the authority's own source, so the authority match is the gate. `seek` keeps its gate and calls `seek_external`. The gate-matrix tests for the two deleted calls (`sync.rs:318-357`) are deleted; the play, pause and seek matrices stay. |

### 3.6 Edge cases

| Case | Behaviour after |
|---|---|
| Play / resume | Snap forward from the current time, then advance normally. No repeated frame. Under an external authority the follower seeds (D6). |
| Pause | Index kept. Follower cleared under an external authority. |
| Stop | Index 0. Follower cleared. |
| Seek / scrub while stopped | Coalesced as today (`content_thread.rs:327-400`). Snap forward. The playhead moves at most one frame from the click. |
| Seek while playing | Snap forward, hold one advance: the target frame is shown. |
| External seek (P3) | Nearest frame to the target's map time, no hold, epoch bumped, follower seeded at the source's beat (D4, D6). |
| External snap (P3) | The beat jumps to the source; the index does not move; no epoch bump. Backward snaps reset trigger delivery; time readers see no jump (D6). |
| Loop / wrap | No transport loop region exists. Session loops resolve from the beat (`engine.rs:1854-1890`), clip loops from video time. A future transport loop wraps as a seek. A loop wrap in Live: D22. |
| Punch / count-in | None exist. Automation gesture punch-in/out samples the grid beat (`automation.rs:139-178`). |
| Session launch quantise | Activates on the first frame whose sampled beat is at or after the target (`engine.rs:1907-1980`). D14 makes on-frame targets land on their frame. Under an external authority the targets convert through the segment (D8). |
| MIDI phantom clips | NoteOn runs before the engine tick (`content_thread.rs:604-605`) and stamps the previous frame's grid beat (`live_clip_manager.rs:462`, `:531`). The 5 ms NoteOff guard stays wall (`:761`). |
| Record / arm | Tempo, automation and MIDI recording stamp grid beats and times. |
| Undo during play | Arrangement edits reconcile next tick. Tempo edits follow D11. fps edits follow D10 through the per-tick compare. |
| Project load | `initialize` resets the clock; the timer re-anchors (`content_commands.rs:255`). The Link offset clears (D15). |
| Export start / end | D9: export clock first, then seeks, warm-up and frames on the export grid. The follower is suspended (D8). |
| Export under CLK or timecode | Advances on map beats (D9). Today it freezes. |
| Authority change | Converter swapped: from Internal the follower starts at the current frame's beat; between sources the handover is bumpless; back to Internal the beat is kept (D6, D8). |
| Output latency changed while playing | The next sample reads at the new `R`. The phase error steps by the change and the trim removes it, or it snaps if the change is past the snap threshold (D6, D19). |

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
| Under an external authority the index advances exactly `frames`; the beat is continuous, strictly increasing between snaps and seeks, and within D7's phase and speed bounds of the source (P3) | `external_follow` P3 proofs in section 5 (Proof plan): `link_follows_with_steady_beat_speed`, `clk_follows_without_stair_steps`, `abletonosc_fallback_stays_within_bound`, `timecode_follows_song_seconds` |
| The loop matches its stated constants: no ringing, settle times, trim never past the cap, no wind-up | `external_follow::tests::step_response_matches_the_profile` (5, 10, 20 ms steps, both profiles, settle within D6's times +20%, overshoot under 15%), `trim_never_exceeds_the_cap`, `integral_does_not_wind_up` (2 s at the cap, then overshoot under 20% of the step) |
| A relocation snaps once; an outlier never moves anything | `external_follow::tests::snap_needs_agreeing_fresh_samples`, `stale_projections_never_confirm_a_snap`; `tests/external_follow.rs::relocate_snaps_once` |
| Seeds, handovers and hand-back keep the beat where D6 and D8 say | `external_follow::tests::seed_takes_the_source_beat`, `handover_is_bumpless`; `tests/external_follow.rs::handover_keeps_the_beat_continuous` |
| Tempo changes are followed through the base tempo, not the trim | `tests/external_follow.rs::clk_tempo_change_follows_through_the_estimate` (D20's table as bounds) |
| MIDI clock follows MIDI 1.0 position rules | `midi_clock_sync::tests::first_clock_after_start_is_the_start_position`, `ticks_while_stopped_do_not_move_the_position`, `song_position_then_continue_arms_the_position` |
| The tick estimator ignores single late ticks and widens on real changes | `midi_clock_sync::tests::estimator_ignores_a_single_late_tick`, `estimator_widens_on_a_tempo_change_and_narrows_back` |
| The Link offset is taken on every start, quantum-aligned on a Link start (D15) | `tests/external_follow.rs::link_started_play_aligns_to_the_bar_and_steers`, for BUG-0u1al (Link-initiated Play never caches the Link beat offset) |
| Live tempo never outlives its source and never writes the map (D21) | `tests/external_follow.rs::peers_vanishing_keeps_the_beat_and_the_map`, for BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point); negative gate `rg -n 'add_or_replace_point\(Beats::ZERO' crates/manifold-app/src/content_thread.rs` = 0 |
| Every external source is read at `deadline + L` (D19) | `tests/external_follow.rs::read_instant_includes_output_latency`; `content_thread` asserts `read_at == tick_deadline + L` in `frame_exact_app::same_values_per_index_through_content_thread` |
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
- `snap_moves_the_beat_not_the_time` (D6, P3; listed with the P3 proofs below)

**Export regression:** the export tests above, plus the existing ones untouched: the `param_step_clip_edge.rs` export arm, `crates/manifold-app/src/rt_dynamic_export_tests.rs`, and the scoped GPU proofs `scripts/gpu_scope.py` maps for `content_export.rs`.

### P3 proofs

All CPU and seeded. Engine-level tests live in `crates/manifold-playback/tests/external_follow.rs`, with the source models in `tests/support/sync_sources.rs`. Unit tests sit next to their code; section 4 (Invariants & enforcement) lists them.

**Source models.** One per D7 noise model, seeded with the SplitMix generator of `transport_clock/tests.rs:8-20`:
- Link: an exact beat at any instant, a 50 ppm rate gap, and ±0.2 ms timeline steps every 30 s.
- MIDI clock: 24 PPQN arrival times from an integrated tempo curve (steps and ramps), ±1 ms uniform plus 0.3 ms Gaussian jitter, 0.5% of ticks 4 ms late, an 80 ppm gap.
- AbletonOSC: 10 Hz ± 20 ms reports, each ±10 ms off the true position, ±1 ms network jitter, a known RTT.
- Timecode: song seconds on a free-running metro, with or without 25 fps frame labels, up to 1 ms arrival jitter.

Each model produces raw arrivals (ticks with instants, packets with instants) and feeds them to the real code: D20's estimator, the real AbletonOSC and timecode projections, the real Link offset logic. The model never stands in for the code under test. Phase error is measured against the model's true beat at the read instant.

**Fail first.** P3's commit C2 (section 6 (Phasing), P3) adds these tests against the stopgap and records each red with its measured failure. One helper changes between red and green, as in P2: `feed(engine, sample)`. On the stopgap it does what the content thread does today: `follow_external_beat(beat)`, with `set_external_time_sync(true)` for CLK and timecode. After P3 it calls `set_external_sample`. The engine is driven by P2's `drive` helper with seeded wake jitter (`tests/frame_exact_transport.rs:249-274`). Tests 9 and 10 drive the extracted `external_sync` module (commit C1), so they also fail first on real code.

**Measured each frame after a 5 s warm-up:** phase error in ms; beat speed, as the shown beat's increment over the grid step against the true tempo; the speed change from the previous frame; strict increase of the beat; snaps and seeks, from the follower's events (on the stopgap: frames whose index moved by anything other than `frames`). Bounds are D7's "test bound" columns unless a test says otherwise.

| # | Test | What it asserts | Why it fails on the stopgap |
|---|---|---|---|
| 1 | `link_follows_with_steady_beat_speed` | Map 120 BPM, Link 128 (they disagree), 10 simulated minutes: Link bounds, no snaps, strictly increasing | The beat runs at the map's tempo and the ¾-frame rule moves the index about every 11 frames: speed changes by about 100% on those frames, phase up to 12.5 ms |
| 2 | `clk_follows_without_stair_steps` | 120 and 174 BPM, with and without Link tempo, 10 minutes: CLK bounds | The frozen grid jumps to the last tick: beat steps of 0, 1 or 2 frames' worth, phase up to one tick (20.8 ms at 120) |
| 3 | `clk_tempo_change_follows_through_the_estimate` | D20's table. With Link tempo: no snap, peak ≤ 3 ms, under 1 ms within 7 s. CLK alone: rises with no snap and peak ≤ 20 ms; drops with at most the table's snaps; ramps with no snap and peak ≤ 8 ms. Strictly increasing except at a counted snap | Stair steps, as test 2 |
| 4 | `phase_step_settles_by_trim` | Link source steps by +5, +10, +20 and −20 ms: no snap; under 1 ms within 1.0, 5.0, 8.0 and 8.0 s; speed within the cap; strictly increasing | A 20 ms step lands as a whole-frame jump: the beat is not continuous |
| 5 | `relocate_snaps_once` | Fast profile, Link: +40, +300, −500 ms. Slow profile, AbletonOSC: +300 and −500 ms (a one-beat Live loop at 120 BPM). Exactly one snap within the profile's confirmations; beat within 1 ms (fast) or 8 ms (slow) of the source on the next frame; epoch unchanged. Slow +40 ms: no snap, removed by the trim | The index lands on the nearest frame, up to 8 ms off, and no snap event exists |
| 6 | `abletonosc_fallback_stays_within_bound` | Live 128 against map 120, 10 minutes: AbletonOSC bounds, no snaps | Reports are stamped at drain (about 8 ms mean lag) and nudges past ¾ frame move the index |
| 7 | `timecode_follows_song_seconds` | Unquantised seconds at metros 40.0, 40.1, 33.3, 33.4, 20 and 50 ms, three start phases: the unquantised bounds. LiveMTC frame labels at the same metros: phase ≤ 21 ms, no snaps | The frozen grid is placed per message: holds, then multi-frame steps |
| 8 | `handover_keeps_the_beat_continuous` | CLK stops mid-play and AbletonOSC takes over (Live 128, map 120); Link to CLK; Internal to Link. On the handover frame the beat increment is within the cap of the previous one; no snap when the sources agree. Peers vanish: the beat is kept, the index goes to the first frame at or after `map⁻¹(beat)`, then the beat runs at 120 | After CLK hands to AbletonOSC the grid advances at the map's tempo and nudges move it in whole frames: speed and phase bounds fail |
| 9 | `link_started_play_aligns_to_the_bar_and_steers` | Stop; Link starts play at Link beat 37.3 with MANIFOLD paused at beat 10: the offset is a multiple of 4, MANIFOLD's beat matches Link's modulo 4, and Link steers for one minute within Link bounds. BUG-0u1al (Link-initiated Play never caches the Link beat offset) | The offset stays NaN, so Link never steers and the beat drifts at the map's tempo |
| 10 | `peers_vanishing_keeps_the_beat_and_the_map` | Link at 128 over a one-point map at 120; peers vanish: the map is unchanged (point count and tempo), the beat is continuous, then runs at 120. BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo point) | The stale 128 is written into the beat-0 point and the beat is re-derived from time: it jumps |
| 11 | `read_instant_includes_output_latency` | `L` = 0, 20 and 100 ms: the shown beat equals the source's beat at `deadline + L`, within 0.5 ms for Link and 1 ms for CLK | No `L` exists, and the stopgap shows the source one frame ahead: 16.7 ms off at `L` = 0 |
| 12 | `snap_moves_the_beat_not_the_time` | A +300 ms AbletonOSC relocation with a quantised session launch inside it: the launch activates on the landing frame; transport time stays continuous (the temporal reset rule does not fire); epoch unchanged. A −300 ms relocation resets trigger delivery | The index moves instead, so transport time jumps and the temporal reset fires |

**One-hour runs, follower only.** `tests/external_follow.rs::no_false_snaps_in_an_hour`: each source model drives the real estimator and the follower directly (no engine) for one simulated hour at 60 fps: zero snaps, and D7's bounds hold throughout. The engine-level tests above use 10 simulated minutes to keep the suite fast.

**Regression guards, not fail-first:** `export_under_external_sync_advances` extended to assert map beats (D8, D9); `placement_after_an_external_target_wins_the_tick` rewritten against `set_external_sample`; the arbiter's play, pause and seek gate matrices (`sync.rs`).

**App level, one queued GPU run:** `frame_exact_app::same_values_per_index_through_content_thread` asserts the content thread reads at `tick_deadline + L`. The footer field is checked with a headless PNG (the `ui-headless-png-verification` skill, Opus seat).

**Rig log for Peter's test.** One info line per seed, snap, seek and handover, for example `[Transport] CLK snap −38.2 ms` and `[Transport] handover CLK → AbletonOSC`. While an external source drives, one line every 5 s: `[Transport] CLK lock: phase +0.4 ms, trim +0.03%, base 128.00 BPM, ticks late >2 ms: 3`. That line is how the D7 noise models get checked against Live.

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

- **Entry:** branch `feat/frame-exact-sync` at or after f32e3c5fd, the adversarial review of this revision answered, Peter's answers to Q4 and Q5 (or their defaults). Re-run `rg -n 'STOPGAP\(frame-exact P3\)' crates` (expect 23 in 9 files) and `rg -l 'external_time_sync|follow_external_beat' crates` (expect 9). A different count means stop and list the new sites.
- **Read-back:** audit item 1.10, D6–D8, D11, D15, D18–D22, sections 3.1b and 3.4–3.6, the P3 proofs in section 5 (Proof plan); `midi_clock_sync.rs` whole; `link_sync.rs` whole; `osc_sync.rs:270-440`; `transport_sync.rs:1-340`; `ableton_bridge.rs:641-670`, `:874-915`, `:2420-2462`; `content_thread.rs:575-660`, `:1729-2160`; `content_commands.rs:870-975`. Restate D6's loop and constants, D7's read instant, D20's estimator rules and the forbidden moves.
- **Commits.** Small, in this order. Each passes clippy `--tests` and scoped nextest on the crates it touches, except that C2's tests stay red until the commit named for them.
  1. **C1 — Extract, no behaviour change.** A pure `crates/manifold-playback/src/external_sync.rs` takes the decisions the content thread makes today: the Link offset (`content_thread.rs:2020-2034`), live-tempo bookkeeping (`:1784-1828`) and `apply_resolved_tempo`'s beat-0 write decision (`:2131-2151`). The content thread calls it. Every existing test stays green. This exists so tests 9 and 10 can fail first on real code at CPU level.
  2. **C2 — Red tests.** `tests/external_follow.rs` and `tests/support/sync_sources.rs` with tests 1–12 and the `feed` helper on the stopgap. The commit message lists each test red with its measured failure.
  3. **C3 — The follower.** `external_follow.rs` (section 3.1b (ExternalFollower)) with its unit tests. `LiveTempo` moves in from `transport_clock.rs`. Not wired yet.
  4. **C4 — Engine on the follower; the stopgap deleted.** `TempoSource`, `set_external_sample`, `set_follow_authority`, `seek_external`. Deleted: `external_time_sync` and its setters, `follow_external_beat`, `follow_external_time`, `follow_external`, `defer_external`, `take_pending_external`, `PhaseLock`, `Correction`, `Smoothing`, `observe_external`, `clear_external`, the arbiter's `nudge_time` and `set_external_time_sync`, their mocks (`osc_timecode.rs:73-88`, `midi_clock_sync.rs:1010-1030`, `osc_sync.rs:622-650`, `sync.rs:245-270`) and the P2 stopgap tests (section 4). `tests/trigger_source_switch.rs` moves to `set_follow_authority`. The content thread hands over today's source values as samples, stamped as today. `feed` flips. Tests 1, 4, 5, 8 and 12 turn green.
  5. **C5 — MIDI clock.** D20: timestamped events through `mpsc`, MIDI 1.0 position rules, the adaptive estimator, `sample_at(R)`. The packed atomic state, the transport-time integrator and the old BPM estimator are deleted. Tests 2 and 3 turn green.
  6. **C6 — AbletonOSC and timecode.** Arrival stamps on both receive threads, `f64` song seconds, the half-frame bias, `sample_at(R)`, Live's tempo for AbletonOSC. `NudgeBeats` and the timecode nudge branch are deleted. Tests 6 and 7 turn green.
  7. **C7 — Link start and the tempo plane.** D15 offsets at `R` with quantum rounding on a Link start; D21's priority with the 1% guard; live tempo per tick; the Internal beat-0 write deleted. Tests 9 and 10 turn green.
  8. **C8 — Output latency.** D19: the `UserPrefs` key, the footer field, `ContentCommand::SetOutputLatency`, `R = deadline + L`. Test 11 turns green. Headless PNG of the footer.
  9. **C9 — Logs and gates.** The rig log lines, the app-level read-instant assertion, the negative gates, the one-hour runs, this doc's status line.
- **Gate:**
  - `cargo nextest run -p manifold-playback`;
  - `cargo nextest run -p manifold-app content_thread content_commands frame_timer`;
  - `cargo nextest run -p manifold-ui` for the footer field;
  - clippy `--tests` on the three crates;
  - `frame_exact_app::same_values_per_index_through_content_thread`, one queued GPU run through `scripts/gpu_proofs_gate.py`.
  - Negative: `rg -n 'STOPGAP\(frame-exact P3\)|external_time_sync|follow_external|defer_external|PhaseLock|enum Correction|NudgeBeats|fn sync_time_from_beat|fn set_beat' crates` = 0.
- **Demo:** L4 on Peter's rig. The launch command for the branch build goes with the request.
  - MIDI clock with Link on, 5 minutes, including a tempo ramp and a scene with a tempo jump. The 5-second lock lines stay under 1 ms and no snap appears. LFO motion is smooth where it stair-stepped before.
  - The same with Link off, to see the CLK-only numbers and how a big tempo drop looks (Q5).
  - Link: start from Ableton after a Stop. MANIFOLD follows, and its bar lines up with Ableton's.
  - Output latency: the flash-and-click film from D19, then set `L`.
  - Kill the MIDI clock mid-play with AbletonOSC on: the handover line appears and the beat carries on without a visible jump.
  - The click-script goes in the landing merge commit.
- **Forbidden:** holding, skipping or jumping the index for phase; filtering phase inside a source controller (only D20's estimator lives there); a beat in `TransportClock`; trimming time; re-tuning the 2 s and 0.5 s seek thresholds; per-frame measurement of output latency; keeping any deleted stopgap path beside the follower.
- **Size:** about 900 lines changed plus 900 lines of tests and source models.

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

Total estimate: P1 and P2 landed. P3 is now about 900 lines changed plus 900 of tests and source models, up from 450 plus 350: the follower, the MIDI estimator, receive-thread stamps, the output-latency preference and the extracted `external_sync` replace the persistence rule. P4 is about 150 plus 100.

## 7. What may look or feel different on stage

- **Repeatable under MANIFOLD's own clock after P1+P2.** Automation, LFOs, clip cuts and launches land on the same frame each time. Video and water are not yet covered (D16).
- **On-frame cuts move one frame earlier where they were late.** At 150 BPM on a 60 fps project, about 40% of on-beat cuts showed one frame late. They now land on their frame, in live and in export.
- **Play and resume don't repeat a frame.** A seek while playing shows the frame you seek to, then moves on.
- **Seeks land up to one frame after the click.** Invisible at normal zoom.
- **Under external sync, P1+P2 behave as today,** rounded to the nearest frame. Still stair-stepped under MIDI clock until P3.
- **After P3, the beat follows Ableton smoothly.** Under Link and MIDI clock it stays within about a millisecond of Ableton's beat. No held or skipped frames, no MIDI clock stair-steps. A tempo change or ramp in Ableton is followed at once.
- **Corrections are invisible.** Drift is removed by speeding up or slowing down by at most 0.5% for a few seconds. A real jump in Ableton (a loop wrap or relocation seen over AbletonOSC or timecode) moves the beat in one go. Trails, feedback and water carry on through a small jump; a big one is a seek, as today.
- **Visuals sit about 10–17 ms later than today until output latency is set** (Q4). Today's code shows the source up to a frame early by accident. Set "Out" in the footer once per rig.
- **MIDI clock without Link: a sudden big tempo drop runs ahead, then snaps back once** (Q5). With Link on, as on Peter's rig, it doesn't.
- **If MIDI clock dies mid-set and AbletonOSC takes over, the beat carries on without a jump,** now following Live's tempo.
- **Link starts on the bar.** A Link-initiated play may move MANIFOLD's start by up to half a bar so its bar lines up with Ableton's. Link steers again after every Stop.
- **When Link peers vanish, the project's own tempo resumes** at the same beat. The last Link tempo no longer rewrites the project's tempo.
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
6. External sources steer the beat by a small, slow, capped tempo trim on the follow segment, never by holding, skipping or jumping frames. The index always advances by `frames`. Real relocations snap the beat; big ones seek. Peter, 2026-10-14.
7. Sources are read at the frame's screen instant from timestamped arrivals, projected per source.
8. While Ableton drives, the beat follows Ableton's tempo through the follow segment; time stays on the grid; the map is the truth under Internal. Peter, 2026-10-13, extended to all four sources 2026-10-14.
9. On a Link-initiated play the offset rounds to Link's quantum. Peter, 2026-10-13.
10. Export shares the clock, set before the first seek; live equals export, not bit-identity with old export.
11. `project.settings.frame_rate` is the only fps; the engine and the timer follow it.
12. A frame's beat is taken at `time + 1e-7 × step` for comparisons; actions on the frame are recorded at its exact beat.
13. Physics and liquid stay outside the determinism claim.
14. P1+P2 land first with a named external-sync stopgap; P3 after Peter's rig test; P4 separate.
15. Export runs on the song-time-0 grid: frame k is the first export-grid frame at or after the start, plus k. Peter, Q2.
16. Output latency is one number per rig: a machine preference, default 0, applied to every external source, never measured per frame. Peter, 2026-10-14.

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
| D7's noise models are assumptions until the rig log measures Live's MIDI clock jitter, AbletonOSC skew and Link's timeline steps | P3's rig log shows a source outside its model: retune that profile from the log |
| Video playback rate follows `base_bpm`, not the trimmed rate (at most 0.5% apart) | P4 makes video sample transport time (D12); revisit only if P4 slips |

## 10. Open questions for Peter

Q1 is D8, Q2 is D17, Q3 is D15. Two feel calls are open; each has a default P3 will build if he doesn't choose.

- **Q4 — Default output latency.** Peter asked for 0. Today's code (P2's stopgap and every build before it) shows Ableton's position up to a frame early by accident, so with 0 the visuals land about 10–17 ms later than they do now until "Out" is set. Options: (a) 0, as asked, set on the rig before the next show; (b) default to 17 ms, one frame at 60 fps, which keeps today's look until you calibrate. Default if no answer: (a).
- **Q5 — MIDI clock without Link on a sudden big tempo drop.** A clock alone can't know the tempo fell until the next tick is late, so MANIFOLD runs ahead by up to about 30 ms (174 → 128) and then snaps back once, a jump of about two frames. With Link on, as on Peter's rig, it doesn't happen. Options: (a) accept it; (b) show a warning when MIDI clock drives with Link off. Default if no answer: (a).
