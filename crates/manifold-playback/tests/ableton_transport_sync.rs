//! Failure-catalog integration harness for `AbletonTransportSync` (P2 of
//! docs/ABLETON_TRANSPORT_SYNC_DESIGN.md).
//!
//! `FakeAbleton` is a pure model of Ableton Live + AbletonOSC: no sockets, no
//! threads, no sleeps, no wall clock — everything is stepped by an explicit
//! `f64` test-time `now`, matching the machine's own contract (§4). Commands
//! delivered to it apply after a configurable scheduler delay (AbletonOSC
//! processes on Live's ~100ms timer); its listeners emit `is_playing`/`tempo`
//! on change and `current_song_time` at ~10 Hz while playing (discrete change
//! while stopped) — the cadence the design's D4 measures against. Fault
//! knobs (`drop_next_inbound`/`drop_next_outbound`) model single-packet UDP
//! loss (§1: "position seek sent exactly once").
//!
//! `Harness` is the driving loop the content thread will run post-P3 (§5):
//! per frame, tick the machine, drain its `OutMsg`s into the fake, step the
//! fake, feed its `ObsEvent`s back as `on_osc_*`, drain+apply `EngineAction`s
//! to a simulated engine transport, and — only when no expectation is
//! pending and CLK is alive — drag the simulated engine beat toward the
//! fake's (dead-reckoned) position. That last rule is the MIDI-clock plane
//! modeled exactly as the content thread runs it today; gating it on
//! `sync_pending()` is D5, and its absence is what produces F1 on stage.
//!
//! One named test per §2 failure-catalog entry, per the P2 brief.

use manifold_playback::transport_sync::{
    AbletonTransportSync, EngineAction, OutMsg, TransportSyncStatus,
};

/// Content-thread frame period used to drive the harness (matches a 60fps-ish
/// cadence; the exact value doesn't matter, only that it's small vs. the
/// scheduler/listener timescales being modeled).
const FRAME_DT: f64 = 0.016;

/// What the fake's listeners/query replies produce, fed into the machine as
/// `on_osc_*` calls by the harness.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ObsEvent {
    IsPlaying(bool),
    SongTime(f32),
    Tempo(f32),
}

/// A pure model of Ableton Live + AbletonOSC. Stepped entirely by `step_to`;
/// no I/O of any kind.
struct FakeAbleton {
    is_playing: bool,
    song_time_beats: f32,
    tempo_bpm: f32,
    /// Last instant `song_time_beats` was advanced to (dead-reckoning anchor).
    last_advance_at: f64,
    /// AbletonOSC/Live's command-processing latency (default 100ms, §1).
    scheduler_delay: f64,
    extra_latency: f64,
    /// Queued inbound commands: (message, apply-at instant).
    inbox: Vec<(OutMsg, f64)>,
    drop_next_inbound: u32,
    drop_next_outbound: u32,
    last_emit_is_playing: Option<bool>,
    last_emit_tempo: Option<f32>,
    /// 10 Hz cadence anchor while playing.
    last_song_time_emit_at: Option<f64>,
    /// Change-gate anchor while stopped.
    last_emit_song_time_value: Option<f32>,
    /// Query replies bypass change-gating entirely (they're not listeners).
    forced_replies: Vec<ObsEvent>,
}

impl FakeAbleton {
    fn new() -> Self {
        Self {
            is_playing: false,
            song_time_beats: 0.0,
            tempo_bpm: 120.0,
            last_advance_at: 0.0,
            scheduler_delay: 0.1,
            extra_latency: 0.0,
            inbox: Vec::new(),
            drop_next_inbound: 0,
            drop_next_outbound: 0,
            last_emit_is_playing: None,
            last_emit_tempo: None,
            last_song_time_emit_at: None,
            last_emit_song_time_value: None,
            forced_replies: Vec::new(),
        }
    }

    fn with_song_time(mut self, beat: f32) -> Self {
        self.song_time_beats = beat;
        self
    }

    fn with_tempo(mut self, bpm: f32) -> Self {
        self.tempo_bpm = bpm;
        self
    }

    fn with_scheduler_delay(mut self, secs: f64) -> Self {
        self.scheduler_delay = secs;
        self
    }

    fn with_playing(mut self, playing: bool) -> Self {
        self.is_playing = playing;
        self
    }

    /// Discard the next `n` inbound (MANIFOLD → Ableton) commands — models a
    /// dropped UDP packet on the command path (F2).
    fn drop_next_inbound(&mut self, n: u32) {
        self.drop_next_inbound += n;
    }

    /// Discard the next `n` outbound (listener/reply) events — unused by the
    /// current catalog but kept as a symmetrical fault knob per the brief.
    #[allow(
        dead_code,
        reason = "symmetrical fault-injection knob required by the P2 brief; no current F-test exercises outbound loss, kept for future scenarios"
    )]
    fn drop_next_outbound(&mut self, n: u32) {
        self.drop_next_outbound += n;
    }

    /// Queue an outbound MANIFOLD → Ableton command; applies after the
    /// scheduler delay (+ any injected extra latency), unless dropped.
    fn deliver(&mut self, msg: OutMsg, at: f64) {
        if self.drop_next_inbound > 0 {
            self.drop_next_inbound -= 1;
            return;
        }
        let apply_at = at + self.scheduler_delay + self.extra_latency;
        self.inbox.push((msg, apply_at));
    }

    /// An Ableton-side button press, independent of any MANIFOLD command
    /// (F5's "press play in the fake").
    fn press_play_locally(&mut self, now: f64) {
        self.advance(now);
        self.is_playing = true;
    }

    /// Advance the playhead to `now` (only moves while playing), matching
    /// the design's dead-reckoning rule (§4).
    fn advance(&mut self, now: f64) {
        if self.is_playing {
            let elapsed = (now - self.last_advance_at).max(0.0) as f32;
            self.song_time_beats += elapsed * self.tempo_bpm / 60.0;
        }
        self.last_advance_at = now;
    }

    /// Apply any commands now due, advance the playhead, and return the
    /// listener/reply events produced at `now`.
    fn step_to(&mut self, now: f64) -> Vec<ObsEvent> {
        self.inbox
            .sort_by(|a, b| a.1.partial_cmp(&b.1).expect("apply-at is never NaN"));
        let i = 0;
        while i < self.inbox.len() && self.inbox[i].1 <= now {
            let (msg, apply_at) = self.inbox.remove(i);
            self.advance(apply_at);
            match msg {
                OutMsg::StartPlaying => self.is_playing = true,
                OutMsg::StopPlaying => self.is_playing = false,
                OutMsg::SetSongTime(beat) => self.song_time_beats = beat,
                OutMsg::QueryIsPlaying => {
                    self.forced_replies.push(ObsEvent::IsPlaying(self.is_playing));
                }
                OutMsg::QuerySongTime => {
                    self.forced_replies
                        .push(ObsEvent::SongTime(self.song_time_beats));
                }
            }
        }
        self.advance(now);

        let mut events = Vec::new();
        if self.last_emit_is_playing != Some(self.is_playing) {
            events.push(ObsEvent::IsPlaying(self.is_playing));
            self.last_emit_is_playing = Some(self.is_playing);
        }
        if self.last_emit_tempo != Some(self.tempo_bpm) {
            events.push(ObsEvent::Tempo(self.tempo_bpm));
            self.last_emit_tempo = Some(self.tempo_bpm);
        }
        if self.is_playing {
            let due = self
                .last_song_time_emit_at
                .is_none_or(|t| now - t >= 0.1);
            if due {
                events.push(ObsEvent::SongTime(self.song_time_beats));
                self.last_song_time_emit_at = Some(now);
                self.last_emit_song_time_value = Some(self.song_time_beats);
            }
        } else if self.last_emit_song_time_value != Some(self.song_time_beats) {
            events.push(ObsEvent::SongTime(self.song_time_beats));
            self.last_emit_song_time_value = Some(self.song_time_beats);
        }
        events.append(&mut self.forced_replies);

        if self.drop_next_outbound > 0 && !events.is_empty() {
            let drop_n = (self.drop_next_outbound as usize).min(events.len());
            self.drop_next_outbound -= drop_n as u32;
            events.drain(0..drop_n);
        }
        events
    }
}

/// What a frame produced, for tests that need to inspect (not just apply)
/// the machine's effects.
struct FrameResult {
    out: Vec<OutMsg>,
    actions: Vec<EngineAction>,
}

/// Ties `AbletonTransportSync` + `FakeAbleton` + a simulated engine transport
/// together into the per-frame driving loop the content thread will run
/// post-P3 (§5 integration seam).
struct Harness {
    machine: AbletonTransportSync,
    fake: FakeAbleton,
    engine_playing: bool,
    engine_beat: f32,
    now: f64,
}

impl Harness {
    /// Builds the machine + fake and runs one zero-duration frame so the
    /// fake's already-set initial state reaches the machine as its first
    /// listener push — mirroring `enable_transport_sync` subscribing while
    /// Ableton already has state. Any incidental relay from that first
    /// observation (the engine defaults to stopped-at-0, which may not match)
    /// is drained away so tests start from a clean slate.
    fn new(fake: FakeAbleton) -> Self {
        let mut h = Self {
            machine: AbletonTransportSync::new(),
            fake,
            engine_playing: false,
            engine_beat: 0.0,
            now: 0.0,
        };
        h.frame(0.0, true);
        h.machine.drain_out().count();
        h.machine.drain_actions().count();
        h
    }

    /// Aligns the harness's simulated engine transport to `(playing, beat)`
    /// without going through `on_local_play`/`on_local_stop` (which would
    /// create a pending expectation) — used to start a scenario from
    /// "already settled in this state" rather than exercising the gesture
    /// that would produce it.
    fn set_engine_transport(&mut self, playing: bool, beat: f32) {
        self.engine_playing = playing;
        self.engine_beat = beat;
        self.machine.on_local_transport(playing, beat, self.now);
        self.machine.drain_actions().count();
    }

    fn local_play(&mut self, beat: f32) {
        self.engine_playing = true;
        self.engine_beat = beat;
        self.machine.on_local_play(beat, self.now);
    }

    fn local_stop(&mut self) {
        self.engine_playing = false;
        self.machine.on_local_stop(self.now);
    }

    /// One frame of the content-thread driving loop: tick → drain `OutMsg`s
    /// into the fake → step the fake → feed its events back as `on_osc_*` →
    /// drain+apply `EngineAction`s to the simulated engine → (only when
    /// settled and CLK alive) drag the engine beat toward the fake's
    /// position → report the engine's new state back for reconciliation.
    fn frame(&mut self, dt: f64, clk_receiving: bool) -> FrameResult {
        self.now += dt;
        if self.engine_playing {
            self.engine_beat += (dt as f32) * self.fake.tempo_bpm / 60.0;
        }

        self.machine.tick(self.now, clk_receiving);
        let out: Vec<OutMsg> = self.machine.drain_out().collect();
        for msg in &out {
            self.fake.deliver(*msg, self.now);
        }

        let events = self.fake.step_to(self.now);
        for ev in events {
            match ev {
                ObsEvent::IsPlaying(p) => self.machine.on_osc_is_playing(p, self.now),
                ObsEvent::SongTime(b) => self.machine.on_osc_song_time(b, self.now),
                ObsEvent::Tempo(t) => self.machine.on_osc_tempo(t, self.now),
            }
        }

        let actions: Vec<EngineAction> = self.machine.drain_actions().collect();
        for action in &actions {
            match action {
                EngineAction::Play => self.engine_playing = true,
                EngineAction::Pause => self.engine_playing = false,
                EngineAction::SeekBeats(b) | EngineAction::NudgeBeats(b) => {
                    self.engine_beat = *b;
                }
            }
        }

        // The MIDI-clock plane: while no OSC expectation is pending and CLK
        // is alive, the content thread's derive_external_beat drags the
        // engine beat toward Ableton's (dead-reckoned) position. Gating this
        // on `sync_pending()` is design D5 — without the gate, this is
        // exactly what produces F1 on stage.
        if !self.machine.sync_pending() && clk_receiving && self.fake.is_playing {
            self.engine_beat = self.fake.song_time_beats;
        }

        self.machine
            .on_local_transport(self.engine_playing, self.engine_beat, self.now);

        FrameResult { out, actions }
    }

    fn run_until_settled(&mut self, clk_receiving: bool, max_frames: u32) -> bool {
        for _ in 0..max_frames {
            self.frame(FRAME_DT, clk_receiving);
            if !self.machine.sync_pending() {
                return true;
            }
        }
        false
    }
}

// ── F1 — Peter's observed stage bug ─────────────────────────────────

/// Play at beat 128 while Ableton's playhead is stopped at beat 9. The
/// engine beat must never regress toward 9 while the expectation is
/// unconfirmed (D5's `sync_pending()` gate on the clock plane), and the fake
/// must converge to ~128 (D11's compound play+seek expectation).
#[test]
fn f1_play_from_cursor_no_dragback() {
    let fake = FakeAbleton::new().with_song_time(9.0).with_tempo(120.0);
    let mut h = Harness::new(fake);

    h.local_play(128.0);
    assert!(
        h.machine.sync_pending(),
        "play-from-cursor must open a pending expectation"
    );

    let mut settled = false;
    for _ in 0..200 {
        h.frame(FRAME_DT, true);
        if h.machine.sync_pending() {
            assert!(
                h.engine_beat >= 128.0 - 1e-3,
                "engine beat regressed to {} while the expectation was still \
                 pending — the clock plane dragged it back (F1)",
                h.engine_beat
            );
        } else {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "machine never settled the play-from-cursor expectation"
    );
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);
    assert!(
        (h.fake.song_time_beats - 128.0).abs() < 1.2,
        "fake did not converge to the target beat: {}",
        h.fake.song_time_beats
    );
}

// ── F2 — single-packet seek loss ────────────────────────────────────

/// The deferred `SetSongTime` — "the least-protected packet" per §1 — is
/// dropped once. The retransmit (D7) must still land the seek.
#[test]
fn f2_seek_survives_packet_loss() {
    let fake = FakeAbleton::new().with_song_time(9.0).with_tempo(120.0);
    let mut h = Harness::new(fake);

    h.local_play(128.0); // StartPlaying now queued; SetSongTime deferred +15ms.

    // First small frame: only StartPlaying is due (below the 15ms deferred
    // threshold), so it's delivered clean.
    h.frame(0.010, true);
    // Now arm the drop for the deferred SetSongTime specifically — the next
    // frame crosses the 15ms threshold and is the first command to go out.
    h.fake.drop_next_inbound(1);
    h.frame(0.010, true);

    let settled = h.run_until_settled(true, 200);
    assert!(settled, "machine never recovered from the dropped seek packet");
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);
    assert!(
        (h.fake.song_time_beats - 128.0).abs() < 1.2,
        "fake did not converge after packet-loss recovery: {}",
        h.fake.song_time_beats
    );
}

// ── F3 — echo misclassification ─────────────────────────────────────

/// A stop is commanded while Ableton is (as observed) still playing. Until
/// the stop actually applies, the fake's continuing observations (still
/// playing, position still advancing) are pre-command echoes, not a real
/// button press — they must never surface as `EngineAction::Play`.
#[test]
fn f3_late_echo_not_external() {
    let fake = FakeAbleton::new()
        .with_playing(true)
        .with_song_time(40.0)
        .with_tempo(120.0)
        .with_scheduler_delay(0.12); // > one 10Hz cadence tick (0.1s)
    let mut h = Harness::new(fake);
    h.set_engine_transport(true, 40.0);

    h.local_stop();
    assert!(h.machine.sync_pending());

    let mut saw_stale_contradiction = false;
    let mut settled = false;
    for _ in 0..40 {
        let result = h.frame(FRAME_DT, true);
        if h.machine.sync_pending() {
            assert!(
                result.actions.is_empty(),
                "pending stop leaked an action from a stale echo: {:?}",
                result.actions
            );
            if h.fake.is_playing {
                saw_stale_contradiction = true;
            }
        } else {
            settled = true;
            break;
        }
    }
    assert!(
        saw_stale_contradiction,
        "test setup never exercised a genuine stale-echo window \
         (fake never reported still-playing while the stop was pending)"
    );
    assert!(settled, "machine never settled the stop expectation");
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);
}

/// The user changes their mind mid-flight: a local stop arrives while a play
/// expectation is still pending (T13). The new gesture must win outright —
/// the abandoned play observation must never ack the stop.
#[test]
fn f3_real_press_in_flight_wins() {
    let fake = FakeAbleton::new().with_song_time(9.0).with_tempo(120.0);
    let mut h = Harness::new(fake);

    h.local_play(128.0);
    assert!(h.machine.sync_pending());

    h.local_stop();
    let out: Vec<_> = h.machine.drain_out().collect();
    assert!(
        out.contains(&OutMsg::StopPlaying),
        "T13 must emit StopPlaying for the reversing gesture: {out:?}"
    );
    for msg in out {
        h.fake.deliver(msg, h.now);
    }

    let settled = h.run_until_settled(true, 200);
    assert!(settled, "machine never settled after the in-flight reversal");
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);
    assert!(
        !h.fake.is_playing,
        "fake must end stopped — the abandoned play expectation must not have acked"
    );
}

// ── F4 — drift-detection feedback ───────────────────────────────────

/// Settled and playing in lock. The engine beat wanders around (as a clock
/// nudge would move it) via `on_local_transport` alone — this must never be
/// read as a seek gesture; only explicit `on_local_seek` may command
/// Ableton's position (D6).
#[test]
fn f4_clock_drag_never_seeks_ableton() {
    let fake = FakeAbleton::new()
        .with_playing(true)
        .with_song_time(50.0)
        .with_tempo(120.0);
    let mut h = Harness::new(fake);
    h.set_engine_transport(true, 50.0);
    assert!(!h.machine.sync_pending());
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);

    for drift_beat in [48.0, 52.0, 45.0, 60.0, 50.0] {
        h.now += FRAME_DT;
        h.machine.on_local_transport(true, drift_beat, h.now);
        h.machine.tick(h.now, true);
        let out: Vec<_> = h.machine.drain_out().collect();
        assert!(
            !out.iter().any(|m| matches!(m, OutMsg::SetSongTime(_))),
            "clock-plane drift to beat {drift_beat} emitted a seek to Ableton: {out:?}"
        );
    }
    assert!(!h.machine.sync_pending());
}

// ── F5 — amputated inbound relay ────────────────────────────────────

/// With CLK absent throughout, an Ableton-side play (not commanded by
/// MANIFOLD) must still relay into the engine, and subsequent song-time
/// reports must drive position via Nudge/Seek — the D10 full takeover.
#[test]
fn f5_ableton_play_relays_when_clk_absent() {
    let fake = FakeAbleton::new().with_song_time(20.0).with_tempo(120.0);
    let mut h = Harness::new(fake);
    h.set_engine_transport(false, 20.0);

    h.frame(FRAME_DT, false);
    assert_eq!(h.machine.status(), TransportSyncStatus::DegradedOscOnly);

    // Ableton-side play press, independent of MANIFOLD.
    h.fake.press_play_locally(h.now);
    let result = h.frame(FRAME_DT, false);
    assert!(
        result.actions.contains(&EngineAction::Play),
        "CLK-absent inbound play must relay into the engine: {:?}",
        result.actions
    );
    assert!(
        h.engine_playing,
        "harness engine should reflect the applied relay"
    );

    let mut saw_position_action = false;
    for _ in 0..15 {
        let result = h.frame(FRAME_DT, false);
        if result
            .actions
            .iter()
            .any(|a| matches!(a, EngineAction::NudgeBeats(_) | EngineAction::SeekBeats(_)))
        {
            saw_position_action = true;
            break;
        }
    }
    assert!(
        saw_position_action,
        "expected OSC-driven position nudge/seek while CLK absent (D10 takeover)"
    );
}

// ── F6 — stale suppress flag / first-edge swallow ───────────────────

/// A freshly reset machine, re-seeded via explicit queries (queries are
/// never change-gated, unlike listeners) — the same shape as a bridge
/// re-enabling transport sync. The first local play afterward must not be
/// swallowed by anything left over from before the reset.
#[test]
fn f6_first_edge_after_enable_not_swallowed() {
    let fake = FakeAbleton::new().with_song_time(9.0).with_tempo(120.0);
    let mut h = Harness::new(fake);

    h.machine.reset();
    h.machine.drain_out().count();
    h.machine.drain_actions().count();

    // Re-enabling transport sync re-queries current state explicitly, since
    // listeners alone won't re-announce values that haven't changed.
    h.fake.deliver(OutMsg::QueryIsPlaying, h.now);
    h.fake.deliver(OutMsg::QuerySongTime, h.now);
    for _ in 0..12 {
        h.frame(FRAME_DT, true);
    }
    assert!(!h.machine.sync_pending());

    h.local_play(128.0);
    let out: Vec<_> = h.machine.drain_out().collect();
    assert!(
        out.contains(&OutMsg::StartPlaying),
        "first play edge after a fresh reset was swallowed (F6): {out:?}"
    );
}

// ── F7 — constants assume idle localhost ────────────────────────────

/// A machine loaded well past AbletonOSC's ~100ms scheduler tick (400ms).
/// The play-from-cursor gesture must still converge via retries, and the
/// measured-RTT EWMA (D8) must have grown past the 0.3s seed by the time of
/// a later gesture — observed as its first retransmit landing later than
/// the seed-derived 0.6s (2×0.3s) baseline.
#[test]
fn f7_slow_scheduler_400ms_still_converges() {
    let fake = FakeAbleton::new()
        .with_song_time(9.0)
        .with_tempo(120.0)
        .with_scheduler_delay(0.4);
    let mut h = Harness::new(fake);

    h.local_play(128.0);
    let settled = h.run_until_settled(true, 300);
    assert!(
        settled,
        "machine never converged under a 400ms scheduler delay (F7)"
    );
    assert_eq!(h.machine.status(), TransportSyncStatus::Locked);
    assert!((h.fake.song_time_beats - 128.0).abs() < 1.2);

    // A second, cleanly-acked gesture (stop) so the RTT EWMA absorbs another
    // real, loaded round trip before the timing check below.
    h.local_stop();
    let settled = h.run_until_settled(true, 60);
    assert!(settled, "the follow-up stop never settled");

    // Now force a retransmit: a gesture whose first command is dropped, so
    // the machine must retry. Measure the elapsed time to the first
    // retransmit — if RTT adaptation (D8) worked, it lands after the
    // 0.3s-seed baseline of 2×0.3s = 0.6s, not at it.
    let send_time = h.now;
    h.local_play(200.0);
    h.fake.drop_next_inbound(1); // swallow this gesture's first sent command

    // The first frame after `local_play` drains the *original* send (not a
    // retransmit) — consume it before looking for the retry.
    let first = h.frame(FRAME_DT, true);
    assert!(
        first.out.contains(&OutMsg::StartPlaying),
        "expected the gesture's initial send on the first frame: {:?}",
        first.out
    );

    let mut retransmit_at = None;
    for _ in 0..300 {
        let result = h.frame(FRAME_DT, true);
        if result.out.contains(&OutMsg::StartPlaying) {
            retransmit_at = Some(h.now);
            break;
        }
    }
    let retransmit_at =
        retransmit_at.expect("expected a retransmit after the dropped command");
    let elapsed = retransmit_at - send_time;
    assert!(
        elapsed > 0.6,
        "retry interval did not grow past the 0.3s-seed baseline (2×RTT=0.6s): \
         first retransmit landed at {elapsed:.3}s after send"
    );
}
