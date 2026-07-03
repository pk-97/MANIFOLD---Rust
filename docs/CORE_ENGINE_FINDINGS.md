# Core Engine Findings — Work Queue

<!-- index: Actionable work queue derived from CORE_ENGINE_MAP.md §13 (2026-07-03). Every finding from the core-engine read as a concrete work item: what's broken, what it means on stage, the fix, how to verify, effort. P0 = broken features (SMPTE receive unwired, MIDI launch quantize inert), P1 = correctness/trust, P2 = performance at project scale, P3 = decisions + hygiene. Status column is the tracker. -->

**Status: OPEN work queue, created 2026-07-03 from the full core-engine read.**
Source of truth for *how the engine works* is `CORE_ENGINE_MAP.md`; this doc is
the *to-do list* derived from its §13, expanded so any future session can pick
up an item and execute it without re-deriving the analysis. Map §-references
point at the mechanism; file:line anchors are as of `ec547c85`.

Update the Status column as items land. When an item ships, add the commit
hash; when Peter rejects one, mark REJECTED with the reason (and mirror it in
the decision-log memory so it isn't re-proposed).

| # | Item | Priority | Status |
|---|---|---|---|
| F1 | Wire the SMPTE/OSC timecode receive path | P0 | OPEN |
| F2 | Make MIDI launch quantization real | P0 | OPEN |
| F3 | Test the external-sync stack (currently zero tests) | P1 | OPEN |
| F4 | Fix the wrong-clock-epoch calls inside `sync_clips_to_time` | P1 | OPEN |
| F5 | Stop mutating serialized settings per-frame (authority + BPM) | P1 | DECISION NEEDED |
| F6 | Fix `suppress_next_transport` staleness | P1 | OPEN |
| F7 | Hot-path allocation + linear-scan cleanup in the engine tick | P2 | OPEN |
| F8 | `ActiveTimelineClipWindow`: wire it or delete it | P2 | DECISION NEEDED |
| F9 | Tempo-map lookup cost on long recorded lanes | P2 | OPEN |
| F10 | Implement live prewarm (currently a stub) | P2 | OPEN |
| F11 | Re-enable AbletonOSC inbound transport (echo-safe) | P3 | OPEN |
| F12 | f32 seams in beat plumbing | P3 | OPEN |
| F13 | Gate live external tempo by authority (or bless the override) | P3 | DECISION NEEDED |
| F14 | Adaptive/measured round-trip windows (seek cooldown, ownership grace) | P3 | OPEN |
| F15 | Session P2 seams (launch-from-stopped flash; phantom vs session-override) | P3 | OPEN |
| F16 | Stable MIDI device identity (ports are positional) | P3 | OPEN |
| F17 | Delete or fix `subscribe_keyed` key scheme | P3 | OPEN |

---

## P0 — broken features (they exist in the UI/design, they do nothing)

### F1 · Wire the SMPTE/OSC timecode receive path (map §13.1)

**What's broken:** `OscSyncController::on_timecode_received` has zero callers,
and `enable_osc` never registers a subscription on the `OscReceiver` (the TODO
at `osc_sync.rs:139-154` says so). `pending_timecode_seconds` is never set,
`is_receiving_timecode` can never become true.

**Consequences (all silent):** timecode position sync does nothing; timecode
transport-follow (arriving = play, silence = pause) does nothing; the OSC arm
of clock-authority auto-detect (`content_thread.rs`, `tick_sync_controllers`)
can never fire. The timecode display stays `--:--:--:--`.

**On stage:** a set that relies on Ableton LiveMTC timecode lock has no video
sync at all. This was a first-class feature of the Unity rig.

**Fix shape:** the receiver's callbacks are `Fn(&str, &[f32])` and can't hold
`&mut osc_sync`, so mirror the `OscParamRouter` pattern: a shared pending slot
(`Arc<Mutex<Option<(Vec<f32>, Seconds)>>>` or an atomic-packed H:M:S:F) that
the subscription closure writes and `tick_sync_controllers` drains into
`on_timecode_received` before `osc_sync.update()`. Subscribe on `enable_osc`,
unsubscribe on `disable_osc` (already written). Note the receiver's
latest-per-address coalescing is fine here — only the newest timecode frame
matters.

**Verify:** unit test the drop-frame math (F3 covers it); integration: feed a
synthetic OSC packet through a bound `OscReceiver`, assert the controller
seeks/plays. Then a real LiveMTC round trip with Ableton.

**Effort:** small (a day including tests). No design questions.

### F2 · Make MIDI launch quantization real (map §13.2)

**What's broken:** launch *positions* never quantize. Chain of causes:
1. midir events always carry `absolute_tick = −1` (`midi_input.rs:495`), so
   `beat_stamp` is always `None` on the hardware path.
2. The engine's `beat_snapped_beat_resolver` and `absolute_tick_resolver`
   delegates are never installed anywhere in the app.
3. So `LiveClipManager::compute_trigger_snap_beat` falls to
   `host.get_beat_snapped_beat()` → the unsnapped raw `current_beat`.

Durations still quantize (`compute_duration_beats` beat-domain arm applies
QuantizeMode); only starts don't. The pending-launch queue
(`activate_due_pending_launches_at_tick`) compares MIDI-tick targets against a
frame counter — vestigial on this path.

**On stage:** QuantizeMode (quarter/beat/bar) looks like it works but pad hits
land exactly where the finger lands. Launch tightness is manual.

**Fix shape (pick in-session):** simplest correct fix is to snap in
`compute_trigger_snap_beat`'s fallback arm: when there's no tick and no stamp,
quantize `current_beat` up to the next grid boundary per
`project.settings.quantize_mode` (ceil, matching the tick path's
`ceil_to_next_grid = true`) — the live-slot scheduler already gates activation
on `current_beat >= start_beat`, so a future-snapped start "arms" the clip and
it begins exactly on the boundary. That mirrors how `SessionRuntime` quantizes
(`ceil_to_boundary`) — consider sharing that helper. Alternative: install the
resolvers from ContentThread; more plumbing, no extra correctness. Also decide
whether to delete the frame-count tick queue or leave it for a future native
clock path (lean delete, per no-transitional-states).

**Verify:** extend `tests/live_clip.rs`: NoteOn at beat 1.37 with Bar quantize
→ phantom clip `start_beat == 4.0` and no renderer start until the playhead
crosses it; NoteOff before the boundary cancels cleanly (the pending-launch /
commit-cancellation path).

**Effort:** small-medium. The behavior choice (snap-forward vs snap-nearest)
should match Ableton: forward (ceil).

## P1 — correctness and trust

### F3 · Test the external-sync stack (map §12)

**What's missing:** zero tests on `SyncArbiter` (the authority gating matrix),
`midi_clock_sync` (packed `AtomicU64` state, BPM estimator, nudge-vs-hard-seek
split, seek-cooldown suppression), `link_sync`, `osc_sync` (timecode→seconds
incl. SMPTE drop-frame), `osc_receiver` (coalescing, bundle recursion),
`osc_param_router`, `transport_controller`, `tempo_recorder`, and the engine's
drift correction + custom-loop boundary enforcement. The whole layer that keeps
MANIFOLD locked to Ableton for a set is verified only live.

**Fix shape:** pure-logic units first, no hardware needed: arbiter matrix
(source×authority×ownership×cooldown), `MidiClockState` pack/unpack round-trip,
BPM estimator over synthetic tick streams (incl. SPP jump reset),
`sync_position_to_playback` nudge/seek decisions against a scripted
`SyncTarget`, `timecode_to_seconds` drop-frame vectors (00:01:00:02 DF, the
minute-boundary skips), OSC encode/decode round-trips via `rosc`. Then engine
integration: drift correction re-seeks a `StubRenderer` that lies about
playback time; custom loop boundary wraps.

**Verify:** `cargo test -p manifold-playback` stays seconds.

**Effort:** medium (a focused session). Highest trust-per-hour item here —
it's also the safety net for F1/F2/F5/F6.

### F4 · Wrong-clock-epoch calls inside `sync_clips_to_time` (map §13.3)

**What's broken:** `sync_clips_to_time` starts clips with
`start_clip(clip, Seconds::ZERO, …)` (`engine.rs`, start loop) even though
`last_realtime_now` is available. Downstream effects of the 0.0 epoch:
`recently_started_times` entry never gates (0.0 is always older than the
window), the video pending-pause deadline (`0.0 + 0.1`) is instantly expired,
and the to-stop dirty-deadline `max(deadline, 0.0 + 0.05)` is always in the
past. `check_preparing_clips` later re-anchors prepare-phase (video) clips
with the real clock, which is why the visible damage is small — but this is
the exact wrong-clock trap `mark_compositor_dirty_now`'s own doc comment warns
about, sitting inside the authority.

**Fix:** pass `Seconds(self.last_realtime_now)` at the start call and anchor
the dirty-deadline off `last_realtime_now`, matching the seek path. Audit the
other `Seconds::ZERO`/`0.0 +` literals in engine.rs while there.

**Verify:** unit: after `sync_clips_to_time` starts a video clip, its
`recently_started_times` entry equals the engine clock; compositor-exclusion
gate actually engages for the first 0.1s (assert `filter_ready_clips` excludes
it on the same tick).

**Effort:** small. Root-cause class fix, not a patch.

### F5 · Serialized settings mutated per-frame outside EditingService (map §13.4) — DECISION NEEDED

**What's happening:** two writes on every tick bypass the mutation gateway and
land in *serialized* fields: `tick_sync_controllers` writes
`project.settings.clock_authority` from the auto-detect (so a manual
`apply_authority_exclusively` is overwritten one frame later), and
`sync_project_bpm_from_current_beat` writes `project.settings.bpm`. Neither
bumps data_version or undo. A save mid-show persists whatever transient
authority/BPM was live at that instant; loading that file later starts with a
stale authority.

**The decision (Peter):** are these runtime state or project state?
- Lean: **runtime**. Move live authority + displayed BPM to engine/runtime
  fields (like `SessionRuntime` — never serialized); keep a *user-chosen*
  authority preference in settings that auto-detect reads but never writes.
  BPM in settings stays the authored tempo, not the live external readout.
- Alternative: keep them in settings but route through explicit non-undoable
  "live edit" writes and exclude them from save-dirty. Weaker — the save-file
  churn class survives.

**Effort:** medium (touches save/load semantics — check the Liveschool fixture
round-trips).

### F6 · `suppress_next_transport` staleness (map §13.12)

**What's broken:** the arbiter sets the flag on *any* gated play/pause; only
the OSC/Ableton senders consume it, and the Play/Pause command handlers clear
it only when a sender is enabled (`content_commands.rs:116-126`). Sequence
that bites: MIDI Clock drives transport with SYNC output off → flag left set →
user enables SYNC → the first real transport change is swallowed, Ableton
misses a play/stop.

**Fix:** clear the flag when senders are disabled or on sender enable
(`enable_sender`), or make it a one-frame token stamped with the frame it was
set (consume-or-expire). Lean: clear on `enable_sender` + clear when no sender
is enabled at the point the arbiter would set it.

**Verify:** unit test in the F3 arbiter matrix.

**Effort:** tiny.

## P2 — performance at project scale (53 layers / 2928 clips is the baseline)

### F7 · Hot-path allocations + linear scans in the tick (map §13.7)

**What's there:** per-frame (playing) the engine clones `(ClipId, usize)` Vecs
in `update_active_clip_playback_rates`, and per-call in `correct_video_drift`,
`seek_active_clips`, `pause_active_clips`, `resume_ready_clips`,
`check_preparing_clips`, `process_pending_pauses`. Worse:
`find_timeline_clip` is a full linear scan over every layer's every clip, and
`update_active_clip_playback_rates` calls it once per active clip per frame —
O(active × total_clips) at 60Hz. `Timeline` already owns a self-healing
clip-id lookup cache; the engine paths don't use it because it needs
`&mut Timeline` (borrow split with the renderer list).

**Fix shape:** pre-allocated scratch Vecs on the engine for the id lists (the
pattern the rest of the file already uses), and route clip resolution through
`timeline.find_clip_by_id` via a split-borrow helper (project is `&mut`
available in all these paths — the conflict is with `self.renderers`, solved
the same way `split_renderer_project` already does).

**Verify:** hot-path audit prompt (`project_hot_path_audit_prompt` memory) +
profiler before/after on the Liveschool fixture.

**Effort:** medium, mechanical.

### F8 · `ActiveTimelineClipWindow` — wire or delete (map §13.8) — DECISION NEEDED

**What's there:** 680 lines + 7 tests of an incremental boundary-cursor
active-clip query, built for exactly the big-project case — never
instantiated. The engine queries the full layer set every frame via
`get_active_clips_at_beat_ref`; `reset_active_clip_window()` is an empty stub.

**Decision:** measure first. If the per-frame timeline query shows up in the
profile at project scale (it may not — per-layer sorted windows are already
decent), wire the window in as the query backend. If it doesn't, delete the
file (it's re-derivable from git; keeping tested-but-dead code violates the
no-illusions rule).

**Effort:** small to measure; medium to wire.

### F9 · Tempo-map walks are linear and per-frame (map §13.9)

**What's there:** `seconds_to_beat_f64` runs on every `advance_time` and walks
all points; recording writes a point every 0.125 beats, so a long
tempo-automated set produces thousands of points → O(points) per frame,
several times per frame (conversions also run in prewarm, clip-time math,
filter path).

**Fix shape:** cache cumulative seconds at each point (rebuild on map
mutation, which is rare), then binary-search both directions. Keep the
converter pure; the cache lives on `TempoMap` behind the existing
`is_sorted`-style dirty flag.

**Verify:** existing tempo tests + a property test (beat↔seconds round-trip
against the linear reference over random maps).

**Effort:** small-medium.

### F10 · Live prewarm is a stub (map §13.10)

**What's there:** `LiveClipManager::append_live_prewarm_candidates` has a TODO
and only returns already-active slots; `LIVE_PREWARM_MAX_UNIQUE_CLIPS`,
`LIVE_PREWARM_RECENT_PRIORITY_COUNT`, `COMBINED_PREWARM_MAX_UNIQUE_CLIPS` are
unused. And it's not even called — `compute_prewarm_candidates` only scans the
timeline window. First MIDI fire of a cold video rides only the 0.02s
recently-started gate.

**On stage:** the first hit of a pad whose video isn't warm can black-flash.
This is the exact moment a VJ tool must not blink.

**Fix shape:** implement the port: candidates = MIDI-mapped layers'
`source_clip_ids` + recently-triggered clips (recency priority), merged into
the timeline set under `COMBINED_PREWARM_MAX_UNIQUE_CLIPS`, fed through the
existing `prewarm_candidates` → `VideoRenderer::pre_warm_from_candidates`
path. The live-burst interval logic (0.1s while within 3s of a trigger)
already exists.

**Verify:** unit on candidate selection; on-rig check with a cold clip + pad
hit.

**Effort:** medium.

## P3 — decisions, hygiene, hardening

### F11 · AbletonOSC inbound transport relay disabled (map §13.11)

The relay was disabled because `is_playing` listener echoes caused play/pause
oscillation (`content_thread.rs`, commented block + TODO). Today AbletonOSC
mode needs MIDI Clock connected for inbound transport. Fix = proper echo
suppression (the bridge already has the 0.5s echo window used outbound; the
inbound path needs a matching sent-state token, not a bare timeout).
Re-enable behind the F3 test net.

### F12 · f32 seams in the beat plumbing (map §13.6)

The accumulator is f64 but CLK beats arrive via `Beats::from_f32`, beat-stamps
are f32, audio-trigger expiry and TempoRecorder track f32 beats. At ~25k beats
(2h at 174 BPM) f32 resolution is ~2ms of beat — inside today's guards but
eroding them. Sweep the seams to f64 (`current_clock_beat` can return f64
trivially; beat_stamp → f64; recorder fields → f64). Mechanical; do alongside
F3 so the estimator tests pin behavior.

### F13 · Live external tempo ignores authority (map §13.5) — DECISION NEEDED

Link-with-peers (or CLK receiving) feeds `get_bpm_at_beat` and therefore
**video playback rates** even when authority is Internal. Deliberate for the
BPM readout; questionable for rate math. Decision: split display tempo from
rate tempo (display always live, rates only under matching authority), or
bless the current behavior in the decision log. On stage today: enabling Link
to "just see" a DJ's tempo silently retimes BPM-stretched video.

### F14 · Round-trip windows are guesses (map §13.14)

`SEEK_COOLDOWN` 0.3s and `OWNERSHIP_GRACE_PERIOD` 0.5s encode a healthy
localhost Ableton round trip. A loaded machine or networked DAW that exceeds
them reintroduces the playhead drag-back they exist to prevent. Cheap
hardening: measure the actual OSC→CLK round trip when MANIFOLD initiates play
(timestamp out, first coherent CLK position in) and scale both windows from
the observed p95. Log when a round trip exceeds the window so it's visible at
soundcheck instead of mid-set.

### F15 · Session P2 seams (map §13.15)

Two fresh-code interactions to pin down:
(a) `session_launch_slot` from stopped transport calls `play()` (full sync —
arrangement clips on that layer start) before the launch registers, then stops
them a call later: a one-gesture renderer start/stop flash. Fix: register the
pending launch (and the override for immediate launches) before `play()`.
(b) A MIDI phantom clip can land on a session-overridden layer: the timeline
query is suppressed for that layer but live refs still merge, so the phantom
plays over the session slot by merge order. Decide the rule (lean: live
phantom wins while held — it's the performer's hands — and the session slot
resumes on commit), then test it in `tests/session_mode.rs`.

### F16 · MIDI device identity is positional (map §13.16)

`device_id` = midir port index and CLK source selection is by index; a
replug mid-show reorders ports, so held-note NoteOff tracking and the CLK
source can attach to the wrong hardware. Fix: key by port *name* (stable on
macOS for the same device), fall back to index; rebind registered devices on
the existing 2s device scan when names move.

### F17 · `subscribe_keyed` is a trap (map §13.13)

`OscReceiver::unsubscribe_keyed` uses `swap_remove`, which invalidates other
keys for the same address. Unused today (only `unsubscribe_all` is called).
Delete the keyed API or make keys stable (slot map / tombstones). Lean delete
until a real second subscriber exists.

---

## Suggested order

1. **F1 + F2** — the two broken features (each small, high stage value).
2. **F3** — the sync test net, which F4/F5/F6/F11/F12 all land on top of.
3. **F4, F6** — small correctness fixes under the new net.
4. **F5, F13** — the two decisions Peter needs to make (settings vs runtime;
   rate-tempo gating), then their implementations.
5. **F7 → F10** — performance block, profiled on the Liveschool fixture.
6. **F8, F11, F12, F14–F17** — as they slot in.

A Fable bug hunt over the map's §13 (the freeze-map phase-2 pattern) can run
independently of this queue — this doc is the *known* work; the hunt looks for
what the read missed.
