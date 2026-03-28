# Stability Audit: Sections 1 & 2

Audited: 2026-03-28
Auditor: Claude Opus 4.6
Scope: Playback engine timing, sync precision, MIDI/OSC live input safety
Goal: Determine if the system can survive 4+ hour live shows without drift, crash, or state leak.

---

## SECTION 1: Playback Engine & Sync Precision

### Q1: Master Beat Position Type

**Type: `f32`** stored in `PlaybackEngine::current_beat` at `crates/manifold-playback/src/engine.rs:93`.

Time is tracked in f64 (`current_time_double: f64` at line 91), then cast down to f32 for `current_time` (line 92) and converted to beats via `update_beat_from_time()` (line 963-971) which calls `TempoMapConverter::seconds_to_beat()`.

- **FINDING [WARNING]** `engine.rs:93` -- `current_beat: f32` has ~7 decimal digits of precision. At 120 BPM, a 4-hour show reaches beat 28,800. At that magnitude, f32 precision is ~0.002 beats (~1ms). This is adequate for visual sync but marginal. The underlying time source `current_time_double: f64` (line 91) has full precision; beat precision is limited by the f32 conversion in TempoMapConverter. For shows longer than 8 hours at high BPM, sub-beat accuracy degrades noticeably.

- **FINDING [VERIFIED SAFE]** `engine.rs:91` -- `current_time_double: f64` is the master time source. f64 has ~15 decimal digits; at 4 hours (14,400s) precision is sub-microsecond. No time drift from the time representation itself.

### Q2: Beat Position Accumulation vs Absolute Computation

**ACCUMULATED via `+=` (f64), then derived to beat.**

`advance_time()` at `engine.rs:432-437`:
```rust
pub fn advance_time(&mut self, dt_seconds: f64) -> f32 {
    self.current_time_double += dt_seconds;
    self.current_time = self.current_time_double as f32;
    self.update_beat_from_time();
    self.current_time
}
```

Time is accumulated frame-by-frame. However, `current_beat` is **derived** from `current_time` via `TempoMapConverter::seconds_to_beat()` each frame (line 963-971). This means beat position is re-derived, not accumulated.

- **FINDING [WARNING]** `engine.rs:432-433` -- `current_time_double` uses f64 accumulation (`+= dt_seconds`). f64 accumulation over 4 hours at 60fps = ~864,000 additions. Each addition introduces ~1 ULP of error (~2e-12s at magnitude 14400). Total accumulated error is bounded at ~1.7 microseconds after 4 hours. This is negligible for visual sync.

- **FINDING [INFO]** `engine.rs:963-971` -- Beat position is NOT accumulated; it is re-derived from `current_time` via `seconds_to_beat()` every frame. This prevents beat-domain drift from compounding. Good design.

However, when external sync is active (`external_time_sync == true`), `advance_time()` is skipped entirely (line 568) and time is set absolutely via `nudge_time()` (line 441-446). This means external sync has zero drift.

### Q3: Frame / Tick Counter Types and Overflow

- **`TickContext::frame_count: i32`** at `engine.rs:56` -- At 60fps, i32 overflows after ~414 days (2^31 / 60 / 86400). This exceeds any single show but could overflow during long-running installations.
- **FINDING [WARNING]** `engine.rs:56` -- `frame_count: i32` overflows after 414 days at 60fps. For permanent installations, this should be u64. For 4-hour shows, safe.

- **`FrameTimer::fps_frame_count: u64`** at `crates/manifold-app/src/frame_timer.rs:14` -- Never overflows.
- **FINDING [VERIFIED SAFE]** `frame_timer.rs:14` -- u64 frame counter. Never overflows in practice.

- **`MidiClockState::position_sixteenths: i32`** at `crates/manifold-playback/src/midi_clock_sync.rs:33` -- Incremented by MIDI clock (0xF8) at 6 ticks per sixteenth. i32 max = 2,147,483,647 sixteenths. At 120BPM, that's ~89,478,485 beats = ~447 days of continuous clock. Reset on MIDI Start (0xFA) and SPP (0xF2).
- **FINDING [VERIFIED SAFE]** `midi_clock_sync.rs:33` -- `position_sixteenths: i32` won't overflow during any single show. Resets on MIDI Start.

- **`drift_correction_count: i32`** at `engine.rs:133` -- Incremented each drift correction. At max 1 correction per 2-second interval, overflows after ~136 years. Safe.
- **FINDING [VERIFIED SAFE]** `engine.rs:133` -- `drift_correction_count: i32` effectively never overflows.

### Q4: Delta Time Calculation

`FrameTimer::consume_tick()` at `crates/manifold-app/src/frame_timer.rs:52-57`:
```rust
let now = Instant::now();
let dt = (now - self.last_tick_time).as_secs_f64();
self.last_tick_time = now;
```

Uses `std::time::Instant` (monotonic clock) with per-frame subtraction. This produces bounded, non-accumulating delta values.

`realtime_since_start()` at `frame_timer.rs:75-77` also uses `Instant::elapsed()`.

- **FINDING [VERIFIED SAFE]** `frame_timer.rs:52-57` -- Delta time derived from monotonic `Instant` subtraction. No float accumulation for dt. Each frame's dt is independently computed.

### Q5: BPM Change Mid-Playback

When BPM changes, `sync_project_bpm_from_current_beat()` at `engine.rs:1111-1128` updates `project.settings.bpm`. The beat position is always re-derived from `current_time_double` via `update_beat_from_time()` → `TempoMapConverter::seconds_to_beat()`, which consults the tempo map.

There is no "anchor + elapsed * bpm" formula that would jump on BPM change. Instead, the TempoMap accumulates segments, and beat position is computed by integrating through all tempo points up to the current time.

- **FINDING [VERIFIED SAFE]** `engine.rs:963-971,1111-1128` -- BPM changes don't cause beat position jumps. Beat is always derived from time through the full tempo map.

### Q6: Loop/Repeat Math

`compute_video_time()` at `crates/manifold-playback/src/video_time.rs:33`:
```rust
let wrapped = source_local_time % loop_len_sec;
```

Also in `engine.rs:1312`:
```rust
return clip.in_point + (source_elapsed % loop_len_sec);
```

- **FINDING [WARNING]** `video_time.rs:33` and `engine.rs:1312` -- Uses `%` (Rust remainder) instead of `t - (t/len).floor() * len` for loop wrapping. Rust's `%` returns the same sign as the dividend. For positive `source_local_time` (which is guaranteed by `.max(0.0)` at line 20) and positive `loop_len_sec` (guaranteed by `> 0.01` guard at line 32), `%` produces the correct result. However, if `source_local_time` somehow becomes negative (e.g., due to float precision near zero), `%` would return a negative value while `floor`-based repeat would return a positive value. The `.max(0.0)` guard prevents this. **Low risk, but diverges from CLAUDE.md's explicit instruction to use the floor-based formula.**

### Q7: Playback State Machine

States defined in `PlaybackState` (referenced at `engine.rs:90`): **Playing**, **Paused**, **Stopped**.

Transitions:
- `Stopped` -> `Playing`: via `play()` (line 366-378)
- `Playing` -> `Paused`: via `pause()` (line 395-400)
- `Playing` -> `Stopped`: via `stop()` (line 380-393)
- `Paused` -> `Playing`: via `play()` (line 366-378)
- `Paused` -> `Stopped`: via `stop()` (line 380-393)
- `Stopped` -> `Stopped`: `stop()` is idempotent
- `Playing` -> `Playing`: guarded by `if self.current_state == PlaybackState::Playing { return; }` (line 367)
- `Paused` -> `Paused`: guarded by `if self.current_state != PlaybackState::Playing { return; }` (line 396)

Re-entrancy protection: `is_ticking: bool` at line 157, checked at `tick()` line 506-508.

- **FINDING [VERIFIED SAFE]** `engine.rs:90,366-400,506-508` -- State machine is complete with guards against redundant transitions and re-entrancy. Cannot get stuck.

### Q8: Pre-allocated Scratch Buffers

All scratch buffers are cleared at the start of each use:
- `stopped_this_tick.clear()` at line 510
- `timeline_active_scratch.clear()` at line 713
- `ready_clips_list.clear()` at line 1633
- `clips_to_stop_drift.clear()` at line 1444
- `compositor_fallback_clips.clear()` at line 1618
- `became_ready_list.clear()` at line 1344
- `stop_buffer.clear()` at line 782
- `prewarm_candidates.clear()` at line 1717

Scheduler `reclaim()` at `scheduler.rs:129-133` returns Vec capacity for reuse; `merged.clear()` at line 73.

- **FINDING [VERIFIED SAFE]** `engine.rs` (multiple lines above) -- All scratch buffers are cleared before use each frame. None grow unboundedly. The `reclaim` pattern correctly returns Vec capacity.

### Q9: .unwrap() Audit in Engine

1. `engine.rs:1073` -- `self.project.as_mut().unwrap()` -- Guarded by `self.project.is_none()` check at line 1062. **Safe.**
2. `engine.rs:1074` -- `self.live_clip_manager.as_mut().unwrap()` -- Guarded by `self.live_clip_manager.is_none()` check at line 1062. **Safe.**
3. `midi_clock_sync.rs:401` -- `self.receiver.as_ref().unwrap()` -- Guarded by `self.receiver.is_none()` check at line 473. **Safe.**
4. `midi_clock_sync.rs:477` -- `self.receiver.as_ref().unwrap()` -- Same guard. **Safe.**

- **FINDING [VERIFIED SAFE]** All `.unwrap()` calls in the playback engine and MIDI clock sync are preceded by explicit None checks.

### Q10: `as` Cast Audit

1. `engine.rs:404` -- `time_double as f32`: f64 → f32 truncation. After 4 hours, `current_time_double` ~14400.0; f32 precision at this magnitude is ~0.001s. Acceptable for visual sync.
2. `engine.rs:434` -- Same as above.
3. `video_time.rs:33` -- `source_local_time % loop_len_sec`: Both f32, no cast issue.

- **FINDING [WARNING]** `engine.rs:404,434` -- `current_time_double as f32` loses precision at high values. After 4 hours, f32 time precision is ~1ms. After 24 hours, ~8ms. This affects video seek accuracy in `compute_video_time` and `correct_video_drift`. The f64 `current_time_double` is preserved separately, so the core accumulator is fine; only consumers using `current_time: f32` are affected.

### Q11: Clip Progress and Duration == 0

Clip progress is computed as `current_beat - clip.start_beat` (elapsed beats) and duration is `clip.duration_beats`. Division by duration is NOT done in the engine; progress is expressed as elapsed beats or elapsed seconds. The scheduler checks `clip.end_beat() - current_beat` for remaining time (scheduler.rs:112) but never divides by duration.

In `compute_video_time` (video_time.rs:25-38), `media_length` is guarded by `> 0.01` and `loop_len_sec` by `> 0.01` before any modulo operation.

In `compute_duration_beats` (live_clip_manager.rs:188), `spb > 0.0` is checked before dividing by it.

- **FINDING [VERIFIED SAFE]** No division by clip duration in the hot path. All denominators are guarded (>0.01 or >0.0 checks). Zero-duration clips cannot cause division by zero.

### Q12: DataVersion Counter

Defined at `crates/manifold-editing/src/service.rs:44` as `data_version: u64`. Incremented on every mutation via `EditingService`.

- **FINDING [VERIFIED SAFE]** `service.rs:44` -- `data_version: u64` overflows after 2^64 increments. At 1000 mutations/second, that's 585 billion years. Cannot overflow.

---

## SECTION 2: MIDI, OSC & Live Input Safety

### Q1: AtomicClockState Packing

Defined at `crates/manifold-playback/src/midi_clock_sync.rs:28`:

Layout (64 bits total):
- Bits 0..31: `position_sixteenths` (i32, stored as u32) -- 32 bits
- Bits 32..47: `clock_tick` (i16, stored as u16) -- 16 bits (only uses values 0-5)
- Bit 48: `is_playing` -- 1 bit
- Bit 49: `has_received_clock` -- 1 bit
- Bits 50..63: unused -- 14 bits

Memory ordering:
- Reads: `Ordering::Acquire` (line 65)
- Writes (CAS): `Ordering::Release` for success, `Ordering::Acquire` for failure (line 71)

- **FINDING [VERIFIED SAFE]** `midi_clock_sync.rs:28-75` -- Correct Acquire/Release ordering ensures MIDI callback writes are visible to content thread reads. The CAS loop in `update()` (line 70-73) guarantees atomicity. All fields fit within 64 bits without overlap.

### Q2: MIDI Callback Thread Safety

The midir callback runs on a CoreMIDI (macOS) / ALSA (Linux) system thread. Communication with the content thread is via:

1. **Note events**: `mpsc::channel` (midi_input.rs:97-99). The callback sends via `event_tx.send()` (line 503). The content thread drains via `event_rx.try_recv()` (line 544). mpsc is thread-safe by design. Non-blocking: `let _ = event_tx.send(evt)` silently drops if receiver is disconnected.

2. **Clock state**: `AtomicClockState` (midi_clock_sync.rs:28). Lock-free CAS updates from the callback (line 142-151). Content thread reads via `Acquire` load (line 65).

- **FINDING [VERIFIED SAFE]** `midi_input.rs:503, midi_clock_sync.rs:65-74` -- No shared mutable state between threads. mpsc for note events, lock-free atomics for clock state. No Mutex on the MIDI thread. The MIDI thread never blocks.

### Q3: Phantom Clip Orphans (NoteOff Never Arrives)

Phantom clips are stored in `LiveClipManager::live_slots: HashMap<i32, TimelineClip>` (live_clip_manager.rs:73). They persist until NoteOff triggers `commit_live_clip()`.

**There is NO timeout or orphan sweep.** If NoteOff never arrives:
- The live slot stays in the HashMap indefinitely
- The clip continues rendering (video freezes on last frame per scheduler.rs:84-93 comment)
- The slot is only cleared by: explicit NoteOff, `clear_all()` on Stop (engine.rs:384-386), or `clear_on_seek()` for large seeks (engine.rs:461-476)

In `ClipLauncher`, NoteOn auto-commits any existing clip for the same (note, device_id) key before creating a new one (clip_launcher.rs:99-112). So pressing the same note again correctly commits the old phantom.

- **FINDING [WARNING]** `live_clip_manager.rs:73` -- No timeout for orphaned phantom clips. If a MIDI cable disconnects while a note is held, the phantom clip persists until transport Stop or a new NoteOn on the same note+device. The live_slot HashMap entry, slot_creation_times entry, and renderer state will leak until Stop. For a 4-hour show, this could leave a few orphaned slots if MIDI disconnects occur, but each orphan is small (~200 bytes). The visual impact is a frozen frame on the affected layer. **Recommendation: add a configurable phantom timeout (e.g., 30 seconds) or sweep on MIDI device disconnect.**

### Q4: 5ms Time Guard

At `clip_launcher.rs:228`:
```rust
if !has_beat_stamp && realtime_now - tracking.creation_time < 0.005 {
    return;
}
```

This is `< 0.005`, meaning NoteOff is rejected if it arrives in *strictly less than* 5ms. At exactly 5.000ms, `0.005 < 0.005` is false, so the NoteOff is accepted.

- **FINDING [VERIFIED SAFE]** `clip_launcher.rs:228` -- The 5ms guard uses strict `<`, meaning NoteOff at exactly 5ms is accepted. No off-by-one issue. Additionally, native-sequenced events with beat stamps bypass this time-based guard entirely and use the deterministic sequence guard instead (lines 235-239).

### Q5: Channel Filtering on NoteOff

At `clip_launcher.rs:220`:
```rust
if tracking.source_channel != midi_channel {
    return;
}
```

NoteOn records `source_channel: midi_channel` in the NoteOffTracking struct (line 174). NoteOff checks the received channel against the stored channel.

- **FINDING [VERIFIED SAFE]** `clip_launcher.rs:174,220` -- NoteOff channel is checked against the NoteOn's stored channel. Different channels cannot interfere.

### Q6: MIDI Device Disconnect Detection

`MidiInputController` stores `MidiDevice` structs containing `midir::MidiInputConnection` (midi_input.rs:58-66). When midir detects a disconnect, the connection's callback stops firing, but the `MidiDevice` struct remains in `registered_devices`.

There is a periodic device scan every ~2 seconds at `content_thread.rs:234-237`:
```rust
if self.time_since_start - self.last_midi_device_scan_time >= 2.0 {
    self.cached_midi_device_names = ...available_source_names();
}
```

However, this only updates the **cached display names** for the UI. It does NOT re-scan and re-register devices in `MidiInputController`.

The `set_device_filter()` method (midi_input.rs:188-217) does scan and register/unregister devices, but this is only called on user action.

- **FINDING [WARNING]** `midi_input.rs:58-66, content_thread.rs:234-237` -- MIDI device disconnect is NOT actively detected. If a device disconnects, its `MidiInputConnection` silently stops producing events. The `registered_devices` Vec retains the dead entry. There is no auto-reconnect logic. The phantom clip issue from Q3 applies if notes were held during disconnect. **Recommendation: periodically compare `registered_devices` port indices against available ports and clean up stale entries. Auto-reconnect matching devices.**

### Q7: Clock Tick Counter Overflow

`MidiClockState::clock_tick: i32` at `midi_clock_sync.rs:34` cycles 0-5 (reset at >=6, line 144). Never grows.

`position_sixteenths: i32` at `midi_clock_sync.rs:33` is the running position counter. Addressed in Section 1 Q3. Resets on MIDI Start.

The BPM estimator's `tempo_accum_ticks: i32` (midi_clock_sync.rs:278) accumulates ticks within a window, then resets to 0 when the window reaches `min_ticks_per_bpm_estimate` (line 629-630). Max window is 96 ticks before reset.

- **FINDING [VERIFIED SAFE]** `midi_clock_sync.rs:34,278` -- Clock tick is cyclic (0-5). BPM estimator ticks reset at window boundary (~96 ticks). Neither can overflow during normal operation.

### Q8: MIDI Clock Phase: Accumulated vs Absolute

The MIDI clock position (`position_sixteenths`) is **accumulated** from 0xF8 clock ticks in the atomic callback (midi_clock_sync.rs:142-150):
```rust
s.clock_tick += 1;
if s.clock_tick >= 6 {
    s.clock_tick = 0;
    s.position_sixteenths += 1;
}
```

It is reset to absolute on MIDI Start (0xFA, line 156) and Song Position Pointer (0xF2, line 183-184).

The sync controller then computes `clock_beat` from `position_sixteenths` and `clock_tick` (midi_clock_sync.rs:665):
```rust
let clock_beat = (pos_sixteenths as f32 + clock_tick as f32 / 6.0) / 4.0;
```

And converts to time via `sync_target.timeline_beat_to_time(clock_beat)` for nudge/seek operations.

- **FINDING [INFO]** `midi_clock_sync.rs:142-150` -- MIDI clock phase is accumulated from ticks, not computed from an absolute reference. This is inherent to the MIDI Clock protocol (24 PPQN relative ticks). SPP messages provide absolute re-anchoring. The transport time integrator at line 669-683 detects jumps (>384 tick delta) and re-anchors. Within normal operation, accumulated position matches the external DAW's position.

### Q9: OSC UDP Receive Buffer

At `osc_receiver.rs:154`:
```rust
let mut buf = [0u8; 65536];
```

The background thread reads one UDP datagram at a time into a fixed 64KB buffer. The `MessageQueue` (line 32-76) stores only the **latest** message per address:

```rust
fn push(&mut self, address: String, values: Vec<f32>) {
    if let Some(existing) = self.latest.get_mut(&address) { ... }
```

This means bursts are automatically deduplicated: only the last value per address survives to dispatch.

- **FINDING [VERIFIED SAFE]** `osc_receiver.rs:154,45-64` -- UDP buffer is 64KB (sufficient for any single OSC packet). The MessageQueue keeps only the latest message per address, so bursts cannot cause unbounded growth. The queue grows at most to the number of unique OSC addresses, which is bounded by project structure.

However, there is one concern:

- **FINDING [INFO]** `osc_receiver.rs:188` -- `queue.lock().push(msg.addr, values)` allocates a new `String` and `Vec<f32>` for each incoming message if the address is new. The `parking_lot::Mutex` lock is held during this allocation. Under extreme OSC burst (many unique addresses), this could briefly block the background thread. In practice, the set of OSC addresses is fixed per project rebuild, so the HashMap entries are created once and updated in-place thereafter.

### Q10: OSC String Parameter Validation

OSC parsing uses `rosc::decoder::decode_udp()` at `osc_receiver.rs:168`. The `rosc` crate handles OSC wire format parsing, which includes proper handling of null-terminated OSC strings per the OSC spec. The address string (`msg.addr`) is a Rust `String`, which is always valid UTF-8 (rosc ensures this).

Float values are extracted by filtering for `OscType::Float` and `OscType::Int` (line 183-186), ignoring string and other types entirely.

- **FINDING [VERIFIED SAFE]** `osc_receiver.rs:168,183-186` -- `rosc` crate validates OSC packets. Addresses are valid Rust strings. Only float/int values are extracted; string parameters are ignored.

### Q11: Rapid MIDI CC / OSC Automation Rate

**MIDI note events**: Capped at 512 per frame via `MAX_NATIVE_CLOCK_EVENTS_PER_FRAME` (midi_input.rs:52). Excess events remain in the mpsc channel for the next frame. No per-event GPU work.

**OSC parameters**: The `OscReceiver::MessageQueue` keeps only the latest value per address (osc_receiver.rs:46-64). So 1000 OSC messages/sec to the same address result in only 1 parameter write per frame (at 60fps). The `OscParamRouter::apply()` (osc_param_router.rs:211-256) drains pending writes linearly.

- **FINDING [VERIFIED SAFE]** `midi_input.rs:52, osc_receiver.rs:46-64` -- MIDI events are capped at 512/frame. OSC deduplicates to latest-per-address. Rapid automation does not cause per-message GPU work or unbounded processing.

### Q12: Clock Source Switch (Internal <-> External)

`TransportController::apply_authority_exclusively()` at `transport_controller.rs:67-96`:
- Sets `project.settings.clock_authority`
- Enables the new source, disables others via `disable_non_authority_sources()`
- Calls `engine.set_external_time_sync(false)` to clear any lingering external time sync flag

`MidiClockSyncController::disable_midi_clock()` (midi_clock_sync.rs:437-458):
- Resets all state: `is_receiving_clock`, `last_is_playing`, BPM estimator, transport time integrator
- Drops the midir receiver connection

`LinkSyncController::disable_link()` (link_sync.rs:69-79):
- Disables the Link session
- Resets peer count and playing state

`OscSyncController::disable_osc()` (osc_sync.rs:163-179):
- Clears receiving state, timecode state, pending values
- Unsubscribes from OscReceiver

- **FINDING [VERIFIED SAFE]** `transport_controller.rs:67-96, midi_clock_sync.rs:437-458` -- Clock source switch cleanly resets all accumulators and state for both old and new sources. `external_time_sync` is explicitly cleared. No stale state leaks between authority switches.

### Q13: Unbounded Collections with Incoming Messages

Potential unbounded collections:

1. **`mpsc::channel` for MIDI notes** (midi_input.rs:117): `std::sync::mpsc::channel()` is unbounded. If the content thread stalls, the midir callback can push unlimited events.
   - **FINDING [WARNING]** `midi_input.rs:117` -- The mpsc channel for MIDI note events is unbounded. If the content thread freezes (e.g., GPU stall), events accumulate without limit. The 512-per-frame drain cap means a 1-second stall at 1000 events/sec would queue ~1000 events (~100 bytes each = ~100KB). This is manageable for brief stalls but could grow under sustained content thread hangs. **Recommendation: use a bounded channel (e.g., 2048 capacity) with try_send that drops on overflow.**

2. **`ClipLauncher::active_note_off_clips: HashMap<(i32, i32), NoteOffTracking>`** (clip_launcher.rs:30): Bounded by the number of simultaneously held MIDI notes (max 128 notes * number of devices). Entries are removed on NoteOff. **Safe.**

3. **`ClipLauncher::last_triggered_clip_id: HashMap<i32, String>`** (clip_launcher.rs:33): Bounded by unique MIDI note numbers (128). Entries are overwritten, not appended. **Safe.**

4. **`LiveClipManager::clip_starts: HashMap<ClipId, RecordingClipStartInfo>`** (live_clip_manager.rs:92): Grows with active recordings, cleared on NoteOff commit or session end. Bounded by simultaneous notes. **Safe.**

5. **`OscReceiver::subscribers: HashMap<String, Vec<OscCallback>>`** (osc_receiver.rs:96): Rebuilt on project load via `OscParamRouter::rebuild()`. Bounded by project structure. **Safe.**

6. **`OscParamRouter::pending: Arc<Mutex<Vec<PendingWrite>>>`** (osc_param_router.rs:48): Drained every frame in `apply()`. Between frames, grows at most by the number of unique OSC addresses that received messages. **Safe.**

7. **`LiveClipManager::pending_by_tick: BTreeMap<i32, Vec<ClipId>>`** (live_clip_manager.rs:80): Entries are consumed when their target tick arrives. Bounded by pending quantized launches (typically <10). **Safe.**

8. **`MidiInputController::native_clock_events_processed_total: i32`** (midi_input.rs:106): This is a telemetry counter that accumulates per-session. It resets on native path transition (line 278). At 512 events/frame * 60fps, overflows after ~1.2 hours.
   - **FINDING [WARNING]** `midi_input.rs:106,367` -- `native_clock_events_processed_total: i32` accumulates via `+= processed as i32`. At maximum throughput (512 events * 60fps = 30720/sec), i32 overflows after ~19.4 hours. With typical MIDI note rates (~50/sec), overflow takes ~497 days. The counter resets on path transition (line 278), but if the native path stays active for a long show, it could wrap. Similarly `native_clock_same_tick_reorders_total: i32` (line 108). These are telemetry-only fields; overflow causes incorrect diagnostic display but no crash or logic error.

---

## Summary of Findings by Severity

### CRITICAL
None found. The playback engine and MIDI/OSC systems do not have bugs that will crash or corrupt state during a show.

### WARNING (degrades over hours)
| # | Location | Issue |
|---|---|---|
| W1 | `engine.rs:93` | `current_beat: f32` precision degrades at high beat counts (28800+ beats = 4hr at 120BPM). Sub-beat precision ~0.002 beats (~1ms). |
| W2 | `engine.rs:404,434` | `current_time_double as f32` loses precision. After 4hr, f32 time precision ~1ms. After 24hr, ~8ms. Affects seek accuracy. |
| W3 | `engine.rs:56` | `frame_count: i32` overflows after 414 days at 60fps. Not a show-stopper but affects installations. |
| W4 | `video_time.rs:33, engine.rs:1312` | Loop wrapping uses `%` instead of `floor`-based repeat. Correct for positive values but diverges from CLAUDE.md convention. |
| W5 | `live_clip_manager.rs:73` | No timeout for orphaned phantom clips on MIDI disconnect. Leaks slot until Stop or same-note NoteOn. |
| W6 | `midi_input.rs:58-66` | No MIDI device disconnect detection or auto-reconnect. Dead connections persist silently. |
| W7 | `midi_input.rs:117` | Unbounded mpsc channel for MIDI note events. Can grow during content thread stalls. |
| W8 | `midi_input.rs:106,367` | Telemetry counters `native_clock_events_processed_total: i32` can overflow after ~19 hours at max throughput. Display-only impact. |

### INFO (worth noting)
| # | Location | Note |
|---|---|---|
| I1 | `engine.rs:963-971` | Beat position is re-derived from time each frame, not accumulated. Good design prevents beat drift. |
| I2 | `midi_clock_sync.rs:142-150` | MIDI clock position is inherently accumulated (protocol design). SPP provides absolute re-anchoring. |
| I3 | `osc_receiver.rs:188` | OSC queue lock held during allocation for new addresses. Bounded by project structure. |

### VERIFIED SAFE
| # | Location | Reason |
|---|---|---|
| S1 | `engine.rs:91` | f64 master time has sub-microsecond precision after 4 hours |
| S2 | `engine.rs:432-433` | f64 accumulation error bounded at ~1.7us after 4 hours |
| S3 | `frame_timer.rs:52-57` | Delta time from monotonic Instant, no accumulation |
| S4 | `engine.rs:963-971,1111-1128` | BPM changes don't cause beat jumps |
| S5 | `engine.rs:90,366-400,506-508` | State machine complete with re-entrancy guard |
| S6 | `engine.rs` (scratch buffers) | All scratch buffers cleared before use each frame |
| S7 | `engine.rs` (unwrap audit) | All unwraps guarded by prior None checks |
| S8 | `service.rs:44` | DataVersion u64 cannot overflow |
| S9 | `midi_clock_sync.rs:28-75` | Atomic clock state: correct Acquire/Release ordering |
| S10 | `midi_input.rs:503, midi_clock_sync.rs:65-74` | No shared mutable state between MIDI and content threads |
| S11 | `clip_launcher.rs:228` | 5ms guard uses strict `<`, no off-by-one |
| S12 | `clip_launcher.rs:174,220` | NoteOff channel filtering correct |
| S13 | `midi_clock_sync.rs:34,278` | Clock tick counters cycle or reset, no overflow |
| S14 | `osc_receiver.rs:154,45-64` | OSC deduplicates to latest-per-address, bounded growth |
| S15 | `osc_receiver.rs:168,183-186` | rosc validates OSC packets, only float/int extracted |
| S16 | `midi_input.rs:52, osc_receiver.rs:46-64` | Rapid automation capped/deduplicated |
| S17 | `transport_controller.rs:67-96` | Clock source switch resets all accumulators cleanly |
| S18 | `frame_timer.rs:14` | u64 fps frame counter never overflows |
| S19 | `engine.rs:133` | drift_correction_count never realistically overflows |
| S20 | Video time division guards | All denominators guarded by >0.01 or >0.0 checks |

---

## Overall Assessment

**The playback engine is well-designed for live performance stability.** The core timing architecture (f64 accumulation with per-frame beat derivation) is sound and will not drift meaningfully during a 4-hour show. The MIDI/OSC input systems use appropriate lock-free and channel-based communication patterns.

The most actionable findings are:
1. **W5/W6 (MIDI disconnect)**: Add device disconnect detection and phantom clip timeout for robustness during cable failures.
2. **W7 (unbounded MIDI channel)**: Switch to bounded channel with overflow drop.
3. **W4 (loop math)**: Consider aligning with the `floor`-based repeat formula specified in CLAUDE.md for consistency.

None of the warnings are show-stopping for a 4-hour performance. The system is stable for its intended use case.
