# Ableton Transport Sync — closed-loop transport between MANIFOLD and Ableton Live

**Status:** APPROVED design, building this session · 2026-07-07 · Fable
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight, in Peter's framing: the current transport sync is
**open-loop control** — every command to Ableton is fire-and-forget, and every
question ("was that inbound message an echo of my own command or a real button
press?") is answered by wall-clock guessing instead of acknowledgment. This
design replaces every timing-window heuristic in the AbletonOSC path with a
**pending-expectation state machine**: a command creates an expectation, inbound
observations are matched against it by value (match = acknowledgment, mismatch =
genuine external event), unacknowledged commands retransmit, and a final timeout
surfaces a sync warning instead of silently fighting. **An unconfirmed
expectation never moves the playhead** — that single rule is what makes the
change safe: worst case degrades to today's behavior, never to a new failure.

Peter's architectural ruling, verbatim (2026-07-07): *"AbletonOSC and a MIDI
Clock connection are both up — actually this is correct. This is what's needed
to sync position via MIDI SPP and keeping everything locked during playback,
tempo changes (with Ableton Link) etc. AbletonOSC only leads to drifts."* The
dual-plane split is therefore **settled**: MIDI Clock is the timing plane
(position lock during playback), AbletonOSC is the command/ack plane
(play/stop/seek + confirmation), Link is the tempo plane. This design fixes the
command plane and its interlock with the timing plane; it does not replace
either plane.

Scope ruling (Peter, 2026-07-07): the M4L device path (`osc_sender.rs`,
`osc_sync` timecode) is **not used anymore** — everything runs through native
AbletonOSC. The M4L path stays untouched as legacy this session (deletion is a
separate cleanup, deferred below). MANIFOLD→Ableton *control* of Live
parameters is explicitly out of scope: *"Don't need to drive Ableton controls
from Manifold yet."*

Companion docs: `CORE_ENGINE_MAP.md` §7/§13 (the authoritative sync-stack map;
its findings 11/12/14 are fixed by this design — update it at landing);
`AUDIO_INFRASTRUCTURE.md` (unrelated audio capture; shares no code).

---

## 1. Audit — what exists (verified 2026-07-07, from a full code read)

| Piece | Where | State |
|---|---|---|
| AbletonOSC bridge | `crates/manifold-playback/src/ableton_bridge.rs` (3279 lines) | Session discovery, macro listeners, cue points, PLAY-group HUD — all healthy, untouched by this design. Transport machinery: lines 43–61 (constants), 377–403 (state fields), 2316–2355 (enable/disable listeners: `is_playing` + `tempo` only), 2379–2404 (`drain_transport`), 2409–2418 (`transport_changed_externally` — **dead code, zero callers**), 2425–2549 (`late_update_transport`) |
| Outbound transport send | `ableton_bridge.rs:2454-2482` | Play/stop sent 3×; **position seek sent exactly once** (line 2512) — the one message carrying *where* is the least-protected packet |
| Echo suppression | `ableton_bridge.rs:45,2478` + `sync.rs:79` | 0.5s wall-clock ignore window + one-shot `suppress_next_transport` boolean (stale-flag hazard = CORE_ENGINE_MAP §13.12) |
| Drift detection | `ableton_bridge.rs:2526-2541` | Dead-reckoned expected-beat vs engine beat; >0.5 beat → send seek. **Feedback hazard: cannot distinguish user seek from clock-plane drag-back**, so it re-sends stale positions to Ableton (the play-from-cursor bug's second half) |
| Inbound transport relay | `content_thread.rs:1582-1589` | **Commented out** ("echo loops → play/pause oscillation"). Ableton-side play reaches MANIFOLD only via MIDI Clock |
| Sync arbiter | `crates/manifold-playback/src/sync.rs` | Authority gate + `manifold_owns_playback` (0.5s grace) + `set_user_seek_time` (0.3s cooldown). Sound structure, keep; the windows it hosts are the open-loop parts |
| MIDI Clock controller | `crates/manifold-playback/src/midi_clock_sync.rs` | Healthy: lock-free receiver, SPP + tick counting, nudge<2.0s/hard-seek position sync (line 726-794), BPM EMA. Its position sync and `set_external_time_sync` are gated on `is_seek_cooldown_active` (lines 587, 622) — the 0.3s guess this design replaces with an ack-based gate |
| Clock-position consumer | `content_thread.rs:1599-1639` (`derive_external_beat`) | MidiClock branch gates on seek-cooldown ONLY — **never checks `manifold_owns_playback`** (Link branch does). This is the missing gate that lets stale clock drag the playhead back after play-from-cursor |
| Play command handler | `content_commands.rs:112-145` | Sets ownership + seek cooldown, then plays. The 0.3s cooldown racing the OSC→Live-scheduler→MIDI round trip is the play-from-cursor bug's first half |
| Transport wiring | `content_thread.rs:623-653` | AbletonOSC mode → `bridge.late_update_transport`; M4L mode → `osc_sender.late_update` (untouched) |
| Authority auto-detect | `content_thread.rs:1464-1499` | CLK > OSC(M4L only) > Link > Internal. AbletonOSC mode hard-codes `osc_receiving = false` — OSC can never take over when CLK dies |
| AbletonOSC (installed) | `~/…/Remote Scripts/AbletonOSC/abletonosc/song.py` | `current_song_time` IS a listenable property (line 57, stock) → `/live/song/start_listen/current_song_time` streams song time at Live's listener cadence (~10 Hz, matches the measured "~10 Hz cadence" in `ableton_bridge.rs:722`). Integer `beat` event listener also available (line 283). The arrangement-clips patch is unrelated to transport |
| M4L sender | `crates/manifold-playback/src/osc_sender.rs` | Legacy, unused by Peter — DO NOT TOUCH |

Extend, don't redesign: the bridge's socket/recv-thread/discovery infrastructure,
the arbiter's authority gate, and the MIDI clock receiver are all kept as-is.

## 2. The failure catalog (what this design must kill)

Each becomes a named harness test in P2. F1 is Peter's observed bug
(screenshot, 2026-07-07); the rest were found in the code read.

- **F1 — play-from-cursor drag-back.** Play at beat 128 (blue cursor) while
  Ableton's playhead is at beat 9 (red). MANIFOLD starts at 128, sends
  `start_playing`; Ableton starts at 9, sprays MIDI clock from 9; the seek to
  128 is in flight. The 0.3s cooldown expires before Live's ~100ms-granularity
  scheduler completes the round trip; clock position (9) drags the engine back;
  drift-detection then sends seek(9) to Ableton, cancelling the user's intent.
  Both loops converge on the wrong answer.
- **F2 — single-packet seek loss.** The deferred position seek is one UDP
  packet. Drop it and F1 happens deterministically.
- **F3 — echo misclassification.** Echo window too short → own echo treated as
  external command (flap). Real button press inside the window → swallowed
  (doesn't respond).
- **F4 — drift-detection feedback.** Any clock-plane drag of the engine beat is
  indistinguishable from a user seek, so it gets sent back to Ableton.
- **F5 — amputated inbound relay.** Ableton-side play/stop only reaches
  MANIFOLD if MIDI Clock is connected and alive.
- **F6 — stale suppress flag.** `suppress_next_transport` set by a gated play
  with no sender enabled is consumed later by the wrong edge (swallows the
  first real play after enabling sync).
- **F7 — constants assume idle localhost.** 0.3s/0.5s windows encode a healthy
  round trip; a loaded show machine exceeding them reintroduces every bug the
  windows exist to prevent.

## 3. Decisions

- **D1 — Dual-plane architecture stays.** MIDI Clock drives position during
  playback; AbletonOSC carries commands and acknowledgments; Link carries
  tempo. Rejected: OSC-only sync (Peter: "AbletonOSC only leads to drifts" —
  10 Hz listener cadence is not a timing plane). Rejected: MIDI-only (no
  command channel; can't seek Ableton).
- **D2 — Pending-expectation state machine, pure module.** All transport
  decision logic moves to a new socket-free, clock-free struct
  (`transport_sync.rs`, §4). Inputs are explicit events with explicit `now`;
  outputs are drained effect lists. The bridge pumps it; tests drive it with
  scripted time. Rejected: fixing the logic in place inside `ableton_bridge.rs`
  as more flags — that is the architecture that produced F1–F7, and it is
  untestable without sockets.
- **D3 — Acks by value-matching, not time-windows.** After commanding a state,
  inbound observations that MATCH the expectation are acknowledgments; inbound
  observations that CONTRADICT a *settled* state are genuine external commands
  and MANIFOLD follows them. The 0.5s echo window, the 0.3s seek cooldown
  (AbletonOSC path), and the bridge's use of `suppress_next_transport` are all
  deleted. (`set_user_seek_time`/cooldown machinery stays in `sync.rs` for the
  untouched M4L path.)
- **D4 — Position confirmation via `current_song_time` listener.** The bridge
  subscribes to it alongside `is_playing`/`tempo`. A play/seek expectation is
  confirmed when observed song time, dead-reckoned forward from its receive
  timestamp at the current tempo, lands within **ε = 1.0 beat** of the target
  (~10 Hz cadence ⇒ up to ~0.3 beats of report lag at 175 BPM; 1.0 absorbs
  cadence + jitter without accepting the wrong bar). ⚠ VERIFY-AT-IMPL: exact
  listener reply address — read `song.py` `_start_listen`; expected to reply as
  `/live/song/get/current_song_time`.
- **D5 — While an expectation is pending, the clock plane does not drive the
  engine.** `derive_external_beat`'s MidiClock branch and the MIDI controller's
  position sync + `set_external_time_sync` gate on
  `suppress = arbiter.is_seek_cooldown_active(now) || bridge.sync_pending()`
  (cooldown kept for the M4L/no-bridge case, ack-release for AbletonOSC).
  Release happens on ack — not on a timer. This kills F1's first half; D6
  kills its second half.
- **D6 — Outbound seeks are explicit, never inferred.** The dead-reckoning
  drift detector is deleted. The content thread tells the machine about user
  gestures directly (`SeekTo`/`SeekToBeat`/play handlers call into it). An
  engine-beat divergence while settled is *by definition* clock-plane
  business, never a reason to command Ableton. Kills F4 at the root.
  Defaulted (not propagated): engine-side loop-wrap jumps do NOT seek Ableton;
  if Peter reports wanting loop-follow on stage, that's a new decision, not a
  bug.
- **D7 — Retransmit until acknowledged, for every command.** Play, stop, AND
  seek: send once, then re-send at `2 × RTT_estimate` until acked, max 4
  retries, then degrade loudly (D9). Each retry also sends the matching query
  (`get/is_playing`, `get/current_song_time`) so a lost *listener* packet
  can't strand a delivered command. Replaces the blind 3× burst; fixes F2.
- **D8 — RTT is measured, not assumed.** Ack round trips feed an EWMA
  (seed 300 ms, clamp [50 ms, 1 s], α = 0.3). All retry/timeout intervals
  derive from it. Fixes F7. Rejected: keeping tuned constants — "tuned to the
  machine it was tuned on" is the F7 failure by construction.
- **D9 — Timeout degrades loudly and adopts reality.** Max retries exhausted →
  the machine surfaces a sync warning (§6, `sync_status`), releases all
  suppression, and adopts Ableton's last observed state rather than fighting.
  Never: silent retry forever, silent give-up, or keeping the engine on an
  unconfirmed position.
- **D10 — Inbound relay returns, arbiter-gated, CLK-priority.** While CLK is
  receiving, CLK remains the transport + position source (its Start/Stop
  already relay Ableton's buttons). When CLK is NOT receiving, AbletonOSC
  claims authority (new tier in auto-detect: CLK > AbletonOSC-transport >
  Link > Internal) and the machine's inbound events drive transport AND
  position — song-time nudge/seek at listener cadence with dead-reckoning
  between reports, same thresholds as the MIDI controller (nudge < 2.0 s,
  hard seek beyond). Peter's ruling on the fallback: **full takeover with a
  visible degraded indicator** — on stage, coarse position beats frozen
  position. Fixes F5.
- **D11 — Play-from-cursor is one compound gesture.** `PendingPlay` carries
  the target beat. `start_playing` and the deferred `set/current_song_time`
  (keep the 15 ms defer — Live needs a tick between play and seek; proven by
  the M4L device this code was ported from) are both under the same
  expectation: confirmed only when *playing at ~target*. Until then, no clock
  position, no drift sends, retransmits per D7.
- **D12 — The suppress flag dies in the AbletonOSC path.** Value-matching
  makes it unnecessary: a MidiClock-sourced play that reaches the engine gets
  reconciled against Ableton's already-playing observed state → no command
  emitted, nothing to suppress. Fixes F6 for the path Peter uses. (M4L path
  keeps the flag; out of scope.)

## 4. The state machine (committed signatures)

New file: `crates/manifold-playback/src/transport_sync.rs`. Pure — **no
sockets, no `Instant::now()`, no engine references**. In-repo precedent for
the shape: the discovery state machine already inside `ableton_bridge.rs`
(phase enum + timeout fields), lifted to a testable module.

```rust
/// Closed-loop transport sync state machine for AbletonOSC mode.
/// Owns NO I/O: callers feed events + `now`, then drain effects.
pub struct AbletonTransportSync {
    state: SyncState,
    observed: ObservedAbleton,
    rtt: RttEstimator,
    out: Vec<OutMsg>,
    actions: Vec<EngineAction>,
    status: TransportSyncStatus,
    /// Engine transport as of the last `on_local_transport` call.
    local_playing: bool,
    local_beat: f32,
}

enum SyncState {
    /// No command in flight. External observations that contradict the
    /// engine are genuine remote commands (relay them, D10).
    Settled,
    /// A command was sent; awaiting a value-matched observation (D3).
    Pending { expect: Expectation, sent_at: f64, last_send: f64, retries: u8 },
}

struct Expectation {
    playing: bool,
    /// Some(beat): gesture carries a position (play-from-cursor, seek).
    beat: Option<f32>,
}

/// Last state observed FROM Ableton via listeners/queries, with receive
/// timestamps for dead-reckoning.
#[derive(Default)]
struct ObservedAbleton {
    is_playing: Option<bool>,
    /// (song_time_beats, received_at)
    song_time: Option<(f32, f64)>,
    tempo_bpm: Option<f32>,
}

struct RttEstimator { ewma_secs: f64 }   // seed 0.3, clamp [0.05, 1.0], α=0.3

pub enum OutMsg {
    StartPlaying,
    StopPlaying,
    SetSongTime(f32),
    QueryIsPlaying,
    QuerySongTime,
}

/// Routed through SyncArbiter by the content thread (never applied directly).
pub enum EngineAction {
    Play,
    Pause,
    SeekBeats(f32),
    NudgeBeats(f32),
}

#[derive(Clone, Copy, PartialEq)]
pub enum TransportSyncStatus {
    /// Command plane idle or confirmed; CLK plane owns position.
    Locked,
    /// Expectation in flight (transient, normally < 1 s).
    Confirming,
    /// CLK absent — OSC driving transport+position at listener cadence (D10).
    DegradedOscOnly,
    /// D9 fired: retries exhausted; adopted Ableton's observed state.
    Warning,
}

impl AbletonTransportSync {
    // ── inputs (engine side) ──
    pub fn on_local_play(&mut self, beat: f32, now: f64);
    pub fn on_local_stop(&mut self, now: f64);
    pub fn on_local_seek(&mut self, beat: f32, playing: bool, now: f64);
    /// Per-frame engine state (after tick) — reconciliation only, never
    /// infers seeks (D6).
    pub fn on_local_transport(&mut self, playing: bool, beat: f32, now: f64);

    // ── inputs (OSC side, from bridge listener drain) ──
    pub fn on_osc_is_playing(&mut self, playing: bool, now: f64);
    pub fn on_osc_song_time(&mut self, beats: f32, now: f64);
    pub fn on_osc_tempo(&mut self, bpm: f32, now: f64);

    /// Retry/timeout pump. Call once per frame.
    pub fn tick(&mut self, now: f64, clk_receiving: bool);

    // ── outputs ──
    pub fn drain_out(&mut self) -> impl Iterator<Item = OutMsg> + '_;
    pub fn drain_actions(&mut self) -> impl Iterator<Item = EngineAction> + '_;

    // ── gates read by the content thread ──
    /// True while Pending: clock plane must not drive the engine (D5).
    pub fn sync_pending(&self) -> bool;
    pub fn status(&self) -> TransportSyncStatus;
}
```

### Transition table (normative — P1 transcribes this, tests quote it)

| # | State | Event | Next state | Effects |
|---|---|---|---|---|
| T1 | Settled | `on_local_play(beat)` | Pending{play, Some(beat)} | emit StartPlaying; schedule SetSongTime(beat) at +15 ms (via tick) |
| T2 | Settled | `on_local_stop` | Pending{stop, None} | emit StopPlaying |
| T3 | Settled | `on_local_seek(beat)` while playing | Pending{play, Some(beat)} | emit SetSongTime(beat) |
| T4 | Settled | `on_local_seek(beat)` while stopped | Pending{stop, Some(beat)} | emit SetSongTime(beat) — ack = observed song_time ≈ beat (Live reports it while stopped) |
| T5 | Pending | observation matches expectation (playing equal AND, if `beat` is Some, dead-reckoned song_time within ε=1.0) | Settled | record RTT sample; status→Locked |
| T6 | Pending | observation contradicts expectation | Pending (unchanged) | none — pre-command state still echoing; retries handle delivery. NEVER relay as external (that's F3) |
| T7 | Pending, `tick` | `now - last_send ≥ 2×RTT` and retries < 4 | Pending{retries+1} | re-emit unacked command(s) + matching query (D7) |
| T8 | Pending, `tick` | retries = 4 exhausted | Settled | status→Warning; emit EngineAction reconciling engine to last observed Ableton state (D9) |
| T9 | Settled | `on_osc_is_playing(p)` where p ≠ engine's playing | Settled | emit Play/Pause EngineAction (inbound relay, D10 — content thread applies only when authority permits) |
| T10 | Settled | `on_osc_song_time(b)` while CLK absent and playing | Settled | dead-reckon; Δ < 2.0 s → NudgeBeats, else SeekBeats (D10) |
| T11 | Settled | `on_osc_song_time(b)` while CLK receiving | Settled | update `observed` only — CLK owns position |
| T12 | any | `on_osc_tempo` | same | update `observed.tempo_bpm` (used for dead-reckoning; engine tempo stays CLK/Link's job) |
| T13 | Pending | new local gesture (play/stop/seek) before ack | Pending{new expectation, retries=0} | emit for the NEW gesture — latest user intent wins; the old expectation is abandoned |

Dead-reckoning rule (T5/T10): `observed_now = song_time + (now − received_at)
× tempo_bpm / 60`, only while `observed.is_playing == Some(true)`.

### The plausible-wrong architecture, forbidden by name

You will want to fix this by adjusting the existing windows — widening the
echo window, lengthening the cooldown, adding one more flag to
`ableton_bridge.rs`. **No.** That is the architecture being deleted; every
constant you'd tune is F7. You will also want the machine to hold a
`&mut PlaybackEngine` or a socket so it can "just do" things — **no**: pure
events in, effects out, or the harness (P2) cannot exist. And you will want to
keep the old time-window code paths alive "as a fallback" — **no**: parallel
old paths are the forbidden move of every seam brief in this repo; the
negative gates below prove deletion.

## 5. Integration seams (P3)

- **Bridge** (`ableton_bridge.rs`): owns an `AbletonTransportSync`.
  `enable_transport_sync` additionally subscribes
  `current_song_time`; `handle_message` routes the three listener addresses
  into machine inputs (timestamped with the frame's `realtime`);
  `late_update_transport`'s body is replaced by: detect engine transport
  edges → `on_local_play`/`on_local_stop` (seeks come explicitly from command
  handlers, D6), then `machine.tick(now, clk_receiving)`, then drain `OutMsg`
  → `send_osc`. **Deleted fields/constants** (negative gate below):
  `TRANSPORT_ECHO_WINDOW_SECS`, `SEEK_THRESHOLD_BEATS`, `TRANSPORT_SEND_COUNT`,
  `CONFIRM_WINDOW_SECS`, `suppress_is_playing_until`, `last_sent_playing`,
  `last_transport_send_time`, `pending_play_seek`, `transport_last_sent_beat`,
  `transport_last_sent_realtime`, `transport_changed_externally` (already
  dead). `PLAY_SEEK_DELAY_SECS` moves into the machine (D11).
- **Command handlers** (`content_commands.rs:112-180`): Play/SeekTo/SeekToBeat
  additionally call the machine's gesture inputs when AbletonOSC transport is
  enabled. The `set_user_seek_time` calls STAY (M4L + no-bridge cases).
- **Content thread** (`content_thread.rs`): the disabled-relay block
  (1582–1589) is replaced by draining `EngineAction`s through the arbiter;
  `derive_external_beat` MidiClock branch and the two `is_seek_cooldown_active`
  gates in `midi_clock_sync.rs` (587, 622) become the combined suppress of D5
  (pass `suppress_position: bool` into `MidiClockSyncController::update` — a
  parameter, not a controller redesign); authority auto-detect (1464–1499)
  gains the AbletonOSC tier of D10.
- **UI surface**: `ContentState` gains
  `ableton_sync_status: TransportSyncStatus` next to the existing
  `ableton_transport_enabled` (content_state.rs); the transport bar shows the
  degraded/warning states. Minimal text treatment this session; visual polish
  belongs to the UI pass.
- **Escalation line**: anything requiring a new thread, channel, or
  `Arc<Mutex>` → stop and escalate (the machine is `&mut self` on the content
  thread; the existing `pending_transport` mutex handoff from the recv thread
  already suffices).

## 6. Phasing

### P1 — the machine + transition tests (Fable, this session)

Entry: this doc. Read-back: §3–§4.
Deliverables: `transport_sync.rs` implementing §4 exactly; in-module unit
tests, one per transition row T1–T13, named `t1_local_play_creates_pending`
style, all with scripted `now` values (no sleeps, no wall clock).
Gate: `cargo test -p manifold-playback --lib transport_sync` green; negative:
`rg 'Instant::now|SystemTime|UdpSocket' crates/manifold-playback/src/transport_sync.rs`
→ zero hits.
Demo: none — L1 (pure logic; behavior demos land in P2/P4).
Forbidden: any I/O in the module; any engine type imported; widening ε or
retry caps beyond §3 without a doc edit.

### P2 — FakeAbleton harness + failure-catalog scenarios (Sonnet)

Entry: P1 merged; `cargo test -p manifold-playback --lib transport_sync` green.
Read-back: §2 (the catalog), §4 transition table, T-row test names from P1.
Deliverables: `crates/manifold-playback/tests/ableton_transport_sync.rs` with a
`FakeAbleton` model (pure struct, stepped by test time: applies commands after
a configurable scheduler delay — default 100 ms; emits listener events at
10 Hz cadence; injectable per-message drop and jitter) and one named test per
catalog entry: `f1_play_from_cursor_no_dragback`, `f2_seek_survives_packet_loss`
(drop every first send; assert convergence via retry),
`f3_late_echo_not_external` / `f3_real_press_in_flight_wins` (T13),
`f4_clock_drag_never_seeks_ableton`, `f5_ableton_play_relays_when_clk_absent`,
`f6_first_edge_after_enable_not_swallowed`,
`f7_slow_scheduler_400ms_still_converges` (RTT adaptation).
F1's test asserts BOTH: engine beat never regresses to the old position, AND
FakeAbleton converges to the target beat.
Gate: `cargo test -p manifold-playback --test ableton_transport_sync` green;
negative: `rg 'sleep|thread::spawn' crates/manifold-playback/tests/ableton_transport_sync.rs`
→ zero hits (fully deterministic).
Demo: the test names + assertions ARE the artifact — L1 by definition, and
that is this phase's ceiling.
Forbidden: real UDP (that's P4's smoke, not P2); asserting on log strings;
relaxing an F-test to pass (a failing F-test means P1 has a bug — escalate).

### P3 — integration: bridge, content thread, arbiter gates, UI status (Sonnet, seam brief §5)

Entry: P1+P2 green. Re-derive the §5 anchors:
`rg -n 'TRANSPORT_ECHO_WINDOW|pending_play_seek|is_seek_cooldown_active|late_update_transport' crates/`
— if counts differ from §1/§5, stop and list before touching.
Deliverables: §5's four seams, compiler-driven (delete the old bridge fields
FIRST; the errors are the checklist).
Gate: workspace `cargo clippy --workspace -- -D warnings`;
`cargo test -p manifold-playback -p manifold-app`; negative:
`rg 'TRANSPORT_ECHO_WINDOW_SECS|CONFIRM_WINDOW_SECS|pending_play_seek|suppress_is_playing_until|transport_changed_externally' crates/`
→ zero hits; `rg 'suppress_next_transport' crates/manifold-playback/src/ableton_bridge.rs`
→ zero hits.
Demo: L2 — a debug-log trace from a real app run (no Ableton needed): enable
AbletonOSC sync with no peer, press play → log shows Pending → retries →
Warning-degrade, playhead advances locally the whole time.
Test scope: full workspace sweep here (this is the final code phase).
Forbidden: touching `osc_sender.rs` or the M4L timecode path; keeping any
old window/flag "just in case"; new shared state.

### P4 — landing + live acceptance (orchestrating session + Peter)

Entry: P3 gate output in hand.
Deliverables: landing per DESIGN_DOC_STANDARD §8.8–10 (`docs/landings/`),
CORE_ENGINE_MAP §7/§13 updated (findings 11/12/14 marked fixed), BUG_BACKLOG
entries for anything found-not-fixed, this doc's Status line updated.
Gate: one real-UDP smoke test (loopback socket to a spawned FakeAbleton
responder) run by the orchestrator; full sweep green.
Demo: L4 — **Peter's live checklist** (the real acceptance gate; no harness
replaces one real round trip under load):
1. Ableton + MANIFOLD up, AbletonOSC sync + MIDI Clock enabled. Press play in
   MANIFOLD at a cursor far from Ableton's playhead → **both** land at
   MANIFOLD's cursor; no jump-back (F1).
2. Press play/stop in Ableton → MANIFOLD follows within a beat, no flap (F5/F3).
3. Scrub MANIFOLD's ruler during playback → Ableton follows, no drag-back.
4. Rapid play-stop-play "drumming" on both sides alternately → converges, no
   oscillation (T13).
5. Tempo ramp in Ableton during playback → position stays locked (D1 planes).
6. Kill the IAC MIDI source mid-playback → HUD shows degraded, video keeps
   following at OSC cadence (D10); restore → Locked returns.
7. Load the machine (export render running) and repeat 1–3 (F7).

## 7. Decided — do not reopen

1. Dual-plane stays: CLK = timing, OSC = commands+acks, Link = tempo (Peter).
2. State machine is a pure module; all I/O stays in the bridge.
3. Acks by value-match; every wall-clock echo/suppression window in the
   AbletonOSC path is deleted, not tuned.
4. Seeks to Ableton come only from explicit user gestures — drift inference
   deleted.
5. Every command retransmits until acked (seek included); RTT measured.
6. Timeout = loud degrade + adopt Ableton's observed reality.
7. CLK-absent fallback = OSC full takeover (transport + position) with visible
   degraded indicator (Peter's ruling, 2026-07-07).
8. M4L path untouched. MANIFOLD→Ableton parameter control out of scope.
9. Engine loop-wraps do not propagate seeks to Ableton (defaulted, D6).

## 8. Deferred

- **Deleting the M4L path** (`osc_sender.rs`, OSC timecode sync, the
  `OscSyncMode::M4L` default in `settings.rs:273`). Trigger: Peter confirms
  after a few shows that AbletonOSC mode covers everything; then a small
  seam-brief cleanup (touches settings serde + default).
- **MANIFOLD→Ableton control** (firing clips/scenes, writing macros). Trigger:
  Peter asks; the bus doctrine conversation (VIDEO_IO) is the likely context.
- **SMPTE/LiveMTC re-wire** (CORE_ENGINE_MAP §13.1). Unrelated to this path;
  belongs to the core-engine fix session already queued in the Opus pack.
- **Adaptive ε / sub-beat position acks** via the integer `beat` listener
  event. Trigger: checklist item 1 shows landing-bar ambiguity at high BPM.
