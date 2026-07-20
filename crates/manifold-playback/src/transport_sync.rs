//! Closed-loop transport sync state machine for AbletonOSC mode.
//!
//! Design contract: docs/ABLETON_TRANSPORT_SYNC_DESIGN.md (§4 signatures,
//! §4 transition table T1–T13). The one-sentence spec: a command to Ableton
//! creates a *pending expectation*; inbound observations are matched against
//! it by value (match = acknowledgment, contradiction while pending = stale
//! pre-command state, contradiction while settled = genuine external command);
//! unacknowledged commands retransmit at a measured-RTT cadence; exhausted
//! retries degrade loudly and adopt Ableton's observed reality.
//!
//! **An unconfirmed expectation never moves the playhead** — while `Pending`,
//! `sync_pending()` is true and the caller must gate the MIDI-clock position
//! plane on it (design D5).
//!
//! Pure by contract: no sockets, no wall clock, no engine types. Callers feed
//! events with an explicit `now` (content-thread realtime seconds) and drain
//! effect lists. This is what makes the P2 harness deterministic.

/// Position-ack tolerance in beats (design D4): listener cadence is ~10 Hz,
/// so a report can lag ~0.3 beats at 175 BPM; 1.0 absorbs cadence + jitter
/// without accepting the wrong bar.
const ACK_EPSILON_BEATS: f32 = 1.0;
/// Delay between start_playing and set/current_song_time (design D11).
/// Live needs a tick between play and seek to accept the position — proven
/// by the M4L device this constant was ported from (seekTask.schedule(10)).
const PLAY_SEEK_DELAY_SECS: f64 = 0.015;
/// Retry cap before the machine degrades loudly (design D7/D9).
const MAX_RETRIES: u8 = 4;
/// Nudge-vs-hard-seek threshold in seconds, matching the MIDI clock
/// controller's position sync (design D10).
const NUDGE_THRESHOLD_SECS: f32 = 2.0;
/// RTT estimator seed/clamp (design D8): pessimistic seed, clamped so a
/// single wild sample can't stall retries or make them spin.
const RTT_SEED_SECS: f64 = 0.3;
const RTT_MIN_SECS: f64 = 0.05;
const RTT_MAX_SECS: f64 = 1.0;
const RTT_EWMA_ALPHA: f64 = 0.3;
/// Dead-reckoning fallback before the first tempo observation arrives.
const FALLBACK_BPM: f32 = 120.0;

// ── Effects ────────────────────────────────────────────────────────

/// Outbound OSC intents, drained by the bridge and encoded there.
/// The machine never touches sockets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutMsg {
    StartPlaying,
    StopPlaying,
    SetSongTime(f32),
    QueryIsPlaying,
    QuerySongTime,
}

/// Engine intents, drained by the content thread and routed through
/// `SyncArbiter` — the machine never mutates the engine directly.
/// Beat→time conversion happens at the consumer (it owns the tempo map).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineAction {
    Play,
    Pause,
    SeekBeats(f32),
    NudgeBeats(f32),
}

/// Sync health, surfaced to the UI (design D9/D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportSyncStatus {
    /// Command plane idle or confirmed; CLK plane owns position.
    Locked,
    /// Expectation in flight (transient, normally < 1 s).
    Confirming,
    /// CLK absent — OSC driving transport + position at listener cadence.
    DegradedOscOnly,
    /// Retries exhausted; adopted Ableton's observed state (D9).
    Warning,
}

// ── Internal state ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Expectation {
    playing: bool,
    /// Some(beat): the gesture carries a position (play-from-cursor, seek).
    beat: Option<f32>,
    /// When `beat` was captured. A PLAYING expectation is a moving target —
    /// "playing, having started from `beat` at `anchored_at`" — because both
    /// engines advance while the ack is in flight. Comparing a frozen beat
    /// against Ableton's advancing position gives a ~ε-wide ack window that
    /// real listener latency blows straight past.
    anchored_at: f64,
}

#[derive(Debug)]
enum SyncState {
    /// No command in flight. External observations that contradict the
    /// engine are genuine remote commands (T9/T10).
    Settled,
    /// A command was sent; awaiting a value-matched observation (T5–T8).
    Pending {
        expect: Expectation,
        /// First-send time — the RTT sample anchor.
        sent_at: f64,
        /// Last (re)send time — the retry timer anchor.
        last_send: f64,
        retries: u8,
        /// Deferred `SetSongTime` not yet emitted (D11): play is sent
        /// immediately, the position follows after PLAY_SEEK_DELAY_SECS.
        /// The position is read from `local_beat` AT FIRE TIME (not frozen
        /// at gesture time) so a late tick — frame hitch — can't send a
        /// stale beat and yank Ableton backwards.
        deferred_seek_owed: bool,
    },
}

/// Last state observed FROM Ableton (listeners + query replies), with
/// receive timestamps for dead-reckoning.
#[derive(Debug, Default, Clone, Copy)]
struct ObservedAbleton {
    is_playing: Option<bool>,
    /// (song_time_beats, received_at)
    song_time: Option<(f32, f64)>,
    tempo_bpm: Option<f32>,
}

impl ObservedAbleton {
    /// One-line snapshot for the `[ABL-SYNC]` diagnostics — the evidence a
    /// live run produces when acks starve.
    fn describe(&self, now: f64) -> String {
        let playing = match self.is_playing {
            Some(true) => "playing",
            Some(false) => "stopped",
            None => "playing=UNOBSERVED",
        };
        let pos = match self.song_time {
            Some((beats, at)) => {
                format!("song_time={beats:.2} ({:.0}ms old)", (now - at) * 1000.0)
            }
            None => "song_time=UNOBSERVED".to_string(),
        };
        let tempo = match self.tempo_bpm {
            Some(bpm) => format!("tempo={bpm:.1}"),
            None => "tempo=UNOBSERVED".to_string(),
        };
        format!("{playing}, {pos}, {tempo}")
    }

    /// Ableton's song position projected to `now` (design §4 dead-reckoning
    /// rule): advance the last report by elapsed time only while playing.
    fn song_time_at(&self, now: f64) -> Option<f32> {
        let (beats, received_at) = self.song_time?;
        if self.is_playing == Some(true) {
            let bpm = self.tempo_bpm.unwrap_or(FALLBACK_BPM);
            Some(beats + ((now - received_at) as f32) * bpm / 60.0)
        } else {
            Some(beats)
        }
    }
}

#[derive(Debug)]
struct RttEstimator {
    ewma_secs: f64,
}

impl RttEstimator {
    fn new() -> Self {
        Self { ewma_secs: RTT_SEED_SECS }
    }
    fn sample(&mut self, rtt: f64) {
        let clamped = rtt.clamp(RTT_MIN_SECS, RTT_MAX_SECS);
        self.ewma_secs += (clamped - self.ewma_secs) * RTT_EWMA_ALPHA;
    }
    /// Retry cadence: 2×RTT (design D7).
    fn retry_interval(&self) -> f64 {
        self.ewma_secs * 2.0
    }
}

// ── The machine ────────────────────────────────────────────────────

pub struct AbletonTransportSync {
    state: SyncState,
    observed: ObservedAbleton,
    rtt: RttEstimator,
    out: Vec<OutMsg>,
    actions: Vec<EngineAction>,
    /// D9 latch — cleared by the next successful ack.
    warning_latched: bool,
    /// CLK plane liveness as of the last tick(); drives Degraded status
    /// and the T10-vs-T11 position-relay split.
    clk_receiving: bool,
    /// Engine transport as of the last `on_local_transport` call.
    local_playing: bool,
    local_beat: f32,
}

impl AbletonTransportSync {
    pub fn new() -> Self {
        Self {
            state: SyncState::Settled,
            observed: ObservedAbleton::default(),
            rtt: RttEstimator::new(),
            out: Vec::new(),
            actions: Vec::new(),
            warning_latched: false,
            clk_receiving: false,
            local_playing: false,
            local_beat: 0.0,
        }
    }

    /// Full reset — bridge calls this on transport-sync enable/disable so a
    /// re-enable never inherits stale expectations (the F6 class).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // ── Inputs: engine side ────────────────────────────────────────

    /// A local play gesture (engine just started playing at `beat`).
    ///
    /// Value-matching (D12): if Ableton is already observed playing near
    /// this position, the edge is the tail of a relayed remote command —
    /// nothing to send. The shortcut only applies while Settled: during
    /// Pending the observation is known-stale (we commanded a change), so
    /// a new gesture always replaces the expectation (T13).
    pub fn on_local_play(&mut self, beat: f32, now: f64) {
        self.local_playing = true;
        self.local_beat = beat;
        if matches!(self.state, SyncState::Settled)
            && self.observed.is_playing == Some(true)
            && self
                .observed
                .song_time_at(now)
                .is_some_and(|b| (b - beat).abs() <= ACK_EPSILON_BEATS)
        {
            return;
        }
        // T1 / T13: latest user intent wins.
        self.begin_pending(
            Expectation { playing: true, beat: Some(beat), anchored_at: now },
            now,
        );
        self.out.push(OutMsg::StartPlaying);
    }

    /// A local stop gesture. Value-matched like play (D12), with the same
    /// Settled-only restriction on the shortcut.
    pub fn on_local_stop(&mut self, now: f64) {
        self.local_playing = false;
        if matches!(self.state, SyncState::Settled)
            && self.observed.is_playing == Some(false)
        {
            return;
        }
        // T2 / T13.
        self.begin_pending(
            Expectation { playing: false, beat: None, anchored_at: now },
            now,
        );
        self.out.push(OutMsg::StopPlaying);
    }

    /// An explicit user seek (ruler scrub, click). NEVER inferred from beat
    /// divergence — that inference is the deleted drift detector (D6).
    pub fn on_local_seek(&mut self, beat: f32, playing: bool, now: f64) {
        self.local_beat = beat;
        // T3 / T4 / T13.
        self.begin_pending(
            Expectation { playing, beat: Some(beat), anchored_at: now },
            now,
        );
        self.out.push(OutMsg::SetSongTime(beat));
    }

    /// Per-frame engine state after tick — reconciliation bookkeeping only.
    pub fn on_local_transport(&mut self, playing: bool, beat: f32, _now: f64) {
        self.local_playing = playing;
        self.local_beat = beat;
    }

    // ── Inputs: OSC side (bridge listener/query drain) ─────────────

    pub fn on_osc_is_playing(&mut self, playing: bool, now: f64) {
        self.observed.is_playing = Some(playing);
        match self.state {
            SyncState::Pending { .. } => self.try_ack(now),
            SyncState::Settled => {
                // T9 — inbound relay: a contradiction while settled is a
                // genuine remote command.
                if playing != self.local_playing {
                    self.actions.push(if playing {
                        EngineAction::Play
                    } else {
                        EngineAction::Pause
                    });
                }
            }
        }
    }

    pub fn on_osc_song_time(&mut self, beats: f32, now: f64) {
        self.observed.song_time = Some((beats, now));
        match self.state {
            SyncState::Pending { .. } => self.try_ack(now),
            SyncState::Settled => {
                // T10/T11: position relays only when the CLK plane is dead —
                // otherwise CLK owns position and this is bookkeeping.
                if !self.clk_receiving
                    && self.local_playing
                    && self.observed.is_playing == Some(true)
                {
                    let delta_beats = (beats - self.local_beat).abs();
                    let bpm = self.observed.tempo_bpm.unwrap_or(FALLBACK_BPM);
                    let delta_secs = delta_beats * 60.0 / bpm;
                    if delta_secs < NUDGE_THRESHOLD_SECS {
                        self.actions.push(EngineAction::NudgeBeats(beats));
                    } else {
                        self.actions.push(EngineAction::SeekBeats(beats));
                    }
                }
            }
        }
    }

    pub fn on_osc_tempo(&mut self, bpm: f32, _now: f64) {
        // T12 — dead-reckoning input only; engine tempo stays CLK/Link's job.
        if bpm > 0.0 {
            self.observed.tempo_bpm = Some(bpm);
        }
    }

    // ── Pump ───────────────────────────────────────────────────────

    /// Retry/timeout/deferred-seek pump. Call once per frame.
    pub fn tick(&mut self, now: f64, clk_receiving: bool) {
        self.clk_receiving = clk_receiving;

        let SyncState::Pending {
            expect,
            sent_at,
            last_send,
            retries,
            deferred_seek_owed,
        } = &mut self.state
        else {
            return;
        };

        // D11: the deferred position follows the play command — at the
        // engine's CURRENT beat, so a late fire can't send a stale one.
        if *deferred_seek_owed && now - *sent_at >= PLAY_SEEK_DELAY_SECS {
            self.out.push(OutMsg::SetSongTime(self.local_beat));
            expect.beat = Some(self.local_beat);
            expect.anchored_at = now;
            *deferred_seek_owed = false;
        }

        // T7: retransmit at 2×RTT until acked.
        if now - *last_send >= self.rtt.retry_interval() {
            if *retries < MAX_RETRIES {
                *retries += 1;
                *last_send = now;
                // A PLAYING position retransmit re-anchors to the engine's
                // CURRENT beat — MANIFOLD's playhead kept advancing, and the
                // gesture means "sync to my playhead", not "return to where
                // it was when I pressed play". Re-sending the stale anchor
                // is the L4-observed bug: every retry visibly yanked
                // Ableton's playhead backwards. A STOPPED seek stays frozen
                // (nothing advances).
                if expect.playing && expect.beat.is_some() {
                    expect.beat = Some(self.local_beat);
                    expect.anchored_at = now;
                }
                let expect = *expect;
                log::info!(
                    "[ABL-SYNC] retry {}/{} — no ack for {{playing={}, beat={:?}}}; observed: {}",
                    retries,
                    MAX_RETRIES,
                    expect.playing,
                    expect.beat,
                    self.observed.describe(now)
                );
                // Re-send the full gesture + the matching queries so a lost
                // LISTENER packet can't strand a delivered command (D7).
                if expect.playing {
                    self.out.push(OutMsg::StartPlaying);
                } else {
                    self.out.push(OutMsg::StopPlaying);
                }
                if let Some(beat) = expect.beat {
                    self.out.push(OutMsg::SetSongTime(beat));
                    self.out.push(OutMsg::QuerySongTime);
                }
                self.out.push(OutMsg::QueryIsPlaying);
            } else {
                // T8: degrade loudly, adopt observed reality (D9).
                log::warn!(
                    "[ABL-SYNC] DEGRADED — {} retries exhausted for {{playing={}, beat={:?}}}; observed: {} — adopting Ableton's state",
                    MAX_RETRIES,
                    expect.playing,
                    expect.beat,
                    self.observed.describe(now)
                );
                self.warning_latched = true;
                if let Some(observed_playing) = self.observed.is_playing
                    && observed_playing != self.local_playing
                {
                    self.actions.push(if observed_playing {
                        EngineAction::Play
                    } else {
                        EngineAction::Pause
                    });
                }
                self.state = SyncState::Settled;
            }
        }
    }

    // ── Outputs ────────────────────────────────────────────────────

    pub fn drain_out(&mut self) -> impl Iterator<Item = OutMsg> + '_ {
        self.out.drain(..)
    }

    pub fn drain_actions(&mut self) -> impl Iterator<Item = EngineAction> + '_ {
        self.actions.drain(..)
    }

    /// FIFO pop variants for call sites that interleave popping with other
    /// `&self` borrows (the bridge's send loop). O(n) shift is fine: the
    /// queue holds a handful of messages at most.
    pub fn pop_out(&mut self) -> Option<OutMsg> {
        if self.out.is_empty() { None } else { Some(self.out.remove(0)) }
    }

    pub fn pop_action(&mut self) -> Option<EngineAction> {
        if self.actions.is_empty() { None } else { Some(self.actions.remove(0)) }
    }

    // ── Gates ──────────────────────────────────────────────────────

    /// True while a command is unconfirmed: the clock plane must not drive
    /// the engine (design D5 — the caller gates `derive_external_beat` and
    /// the MIDI controller's position sync on this).
    pub fn sync_pending(&self) -> bool {
        matches!(self.state, SyncState::Pending { .. })
    }

    pub fn status(&self) -> TransportSyncStatus {
        match self.state {
            SyncState::Pending { .. } => TransportSyncStatus::Confirming,
            SyncState::Settled => {
                if self.warning_latched {
                    TransportSyncStatus::Warning
                } else if !self.clk_receiving {
                    TransportSyncStatus::DegradedOscOnly
                } else {
                    TransportSyncStatus::Locked
                }
            }
        }
    }

    // ── Internals ──────────────────────────────────────────────────

    fn begin_pending(&mut self, expect: Expectation, now: f64) {
        // A play-with-position defers its seek (D11); a bare seek or stop
        // has nothing to defer (seeks send their position immediately in
        // the caller). When replacing an in-flight expectation (T13) the
        // old deferred seek is abandoned with it.
        let deferred_seek_owed = expect.playing && expect.beat.is_some();
        self.state = SyncState::Pending {
            expect,
            sent_at: now,
            last_send: now,
            retries: 0,
            deferred_seek_owed,
        };
    }

    /// T5/T6: check the full expectation against the current observed
    /// snapshot. Match → settle + RTT sample. Contradiction → hold (the
    /// pre-command state echoing back is not an external command).
    ///
    /// A PLAYING position expectation is an interval, not a point: Ableton
    /// starts advancing the moment the command applies, so "acknowledged"
    /// means its observed position is consistent with having started from
    /// the anchor — within [beat − ε, beat + elapsed·tempo + ε]. A STOPPED
    /// expectation stays a point match.
    fn try_ack(&mut self, now: f64) {
        let SyncState::Pending { expect, sent_at, .. } = &self.state else {
            return;
        };
        let playing_matches = self.observed.is_playing == Some(expect.playing);
        let beat_matches = match expect.beat {
            None => true,
            Some(target) => self.observed.song_time_at(now).is_some_and(|b| {
                if expect.playing {
                    let bpm = self.observed.tempo_bpm.unwrap_or(FALLBACK_BPM);
                    let max_advance =
                        ((now - expect.anchored_at).max(0.0) as f32) * bpm / 60.0;
                    b >= target - ACK_EPSILON_BEATS
                        && b <= target + max_advance + ACK_EPSILON_BEATS
                } else {
                    (b - target).abs() <= ACK_EPSILON_BEATS
                }
            }),
        };
        if playing_matches && beat_matches {
            log::info!(
                "[ABL-SYNC] locked — ack in {:.0}ms; observed: {}",
                (now - *sent_at) * 1000.0,
                self.observed.describe(now)
            );
            self.rtt.sample(now - *sent_at);
            self.warning_latched = false;
            self.state = SyncState::Settled;
        }
    }
}

impl Default for AbletonTransportSync {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests: one per transition row (design §4 table) ────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_out(m: &mut AbletonTransportSync) -> Vec<OutMsg> {
        m.drain_out().collect()
    }
    fn drain_actions(m: &mut AbletonTransportSync) -> Vec<EngineAction> {
        m.drain_actions().collect()
    }

    /// A machine that has observed Ableton stopped at beat 9, tempo 120 —
    /// the state right before Peter's play-from-cursor bug (F1).
    fn settled_stopped_at_9() -> AbletonTransportSync {
        let mut m = AbletonTransportSync::new();
        m.tick(0.0, true);
        m.on_osc_tempo(120.0, 0.0);
        m.on_osc_is_playing(false, 0.0);
        m.on_osc_song_time(9.0, 0.0);
        m.drain_out().count();
        m.drain_actions().count();
        m
    }

    #[test]
    fn t1_local_play_creates_pending_and_defers_seek() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        assert!(m.sync_pending());
        assert_eq!(drain_out(&mut m), vec![OutMsg::StartPlaying]);
        // Seek is deferred, not immediate (D11)…
        m.tick(1.010, true);
        assert_eq!(drain_out(&mut m), vec![]);
        // …and emitted after the delay.
        m.tick(1.016, true);
        assert_eq!(drain_out(&mut m), vec![OutMsg::SetSongTime(128.0)]);
    }

    #[test]
    fn t2_local_stop_creates_pending() {
        let mut m = settled_stopped_at_9();
        m.on_osc_is_playing(true, 0.5);
        m.on_local_transport(true, 20.0, 0.5);
        drain_actions(&mut m); // discard the relay from the observation
        m.on_local_stop(1.0);
        assert!(m.sync_pending());
        assert_eq!(drain_out(&mut m), vec![OutMsg::StopPlaying]);
    }

    #[test]
    fn t3_seek_while_playing_sends_song_time() {
        let mut m = settled_stopped_at_9();
        m.on_local_seek(64.0, true, 1.0);
        assert!(m.sync_pending());
        assert_eq!(drain_out(&mut m), vec![OutMsg::SetSongTime(64.0)]);
    }

    #[test]
    fn t4_seek_while_stopped_acks_on_position_alone() {
        let mut m = settled_stopped_at_9();
        m.on_local_seek(64.0, false, 1.0);
        assert_eq!(drain_out(&mut m), vec![OutMsg::SetSongTime(64.0)]);
        // Ableton reports the new position while still stopped → ack.
        m.on_osc_song_time(64.0, 1.2);
        assert!(!m.sync_pending());
        assert_eq!(m.status(), TransportSyncStatus::Locked);
    }

    #[test]
    fn t5_matching_observation_acks_and_samples_rtt() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        m.on_osc_is_playing(true, 1.1);
        assert!(m.sync_pending(), "position not yet confirmed");
        m.on_osc_song_time(128.1, 1.15);
        assert!(!m.sync_pending());
        assert_eq!(m.status(), TransportSyncStatus::Locked);
        // No relay actions from the ack path.
        assert_eq!(drain_actions(&mut m), vec![]);
    }

    /// The ack arrives AFTER Ableton has already played several beats past
    /// the anchor (real listener latency at real tempo). A playing
    /// expectation is an interval, not a point — this must still acknowledge.
    #[test]
    fn t5b_late_ack_after_ableton_advanced_still_acks() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        // A full second later (2 beats at 120 BPM): Ableton reports playing
        // at 130 — consistent with having started from 128.
        m.on_osc_is_playing(true, 2.0);
        m.on_osc_song_time(130.0, 2.0);
        assert!(!m.sync_pending(), "advancing position within the anchor \
             interval must acknowledge (L4 escape regression)");
        assert_eq!(m.status(), TransportSyncStatus::Locked);
    }

    /// The other half of the same escape: an unacked retransmit must seek
    /// Ableton to the engine's CURRENT beat, never back to the stale anchor
    /// (the visible playhead yank-back, once per retry).
    #[test]
    fn t7b_retransmit_seeks_current_beat_not_stale_anchor() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        // Engine keeps playing while the ack is missing.
        m.on_local_transport(true, 131.0, 1.7);
        m.tick(1.7, true); // past 2×RTT seed (0.6s)
        let out = drain_out(&mut m);
        assert!(
            out.contains(&OutMsg::SetSongTime(131.0)),
            "retransmit must carry the current playhead, got {out:?}"
        );
        assert!(
            !out.contains(&OutMsg::SetSongTime(128.0)),
            "retransmit must never re-send the stale anchor (yank-back), got {out:?}"
        );
    }

    #[test]
    fn t6_contradicting_observation_while_pending_is_not_external() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        // Stale pre-command echo: Ableton still reports stopped at 9.
        m.on_osc_is_playing(false, 1.05);
        m.on_osc_song_time(9.0, 1.05);
        assert!(m.sync_pending(), "stale echo must not settle the machine");
        assert_eq!(
            drain_actions(&mut m),
            vec![],
            "stale echo must not relay as an external command (F3)"
        );
    }

    #[test]
    fn t7_unacked_command_retransmits_with_queries() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        // 2×RTT with the 0.3s seed = 0.6s.
        m.tick(1.7, true);
        let out = drain_out(&mut m);
        assert!(out.contains(&OutMsg::StartPlaying));
        assert!(out.contains(&OutMsg::SetSongTime(128.0)));
        assert!(out.contains(&OutMsg::QueryIsPlaying));
        assert!(out.contains(&OutMsg::QuerySongTime));
    }

    #[test]
    fn t8_exhausted_retries_degrade_and_adopt_reality() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        m.on_local_transport(true, 128.0, 1.0);
        drain_out(&mut m);
        // Burn through all retries with no ack.
        let mut now = 1.0;
        for _ in 0..=MAX_RETRIES {
            now += 2.5;
            m.tick(now, true);
        }
        assert!(!m.sync_pending());
        assert_eq!(m.status(), TransportSyncStatus::Warning);
        // Observed reality is "stopped" → engine is told to pause (D9).
        assert_eq!(drain_actions(&mut m), vec![EngineAction::Pause]);
    }

    #[test]
    fn t9_settled_external_play_relays() {
        let mut m = settled_stopped_at_9();
        m.on_local_transport(false, 0.0, 1.0);
        m.on_osc_is_playing(true, 1.5);
        assert_eq!(drain_actions(&mut m), vec![EngineAction::Play]);
        // And the engine edge that follows must NOT echo a command back
        // to Ableton (D12): Ableton reported ~9 while stopped, so the local
        // play at that position value-matches the observation.
        m.on_osc_song_time(9.0, 1.55);
        drain_actions(&mut m);
        m.on_local_play(9.0, 1.6);
        assert_eq!(drain_out(&mut m), vec![], "relay echo must not command Ableton");
        assert!(!m.sync_pending());
    }

    #[test]
    fn t10_osc_position_drives_engine_when_clk_dead() {
        let mut m = settled_stopped_at_9();
        m.tick(1.0, false); // CLK dead
        m.on_osc_is_playing(true, 1.0);
        m.on_local_transport(true, 20.0, 1.1);
        drain_actions(&mut m);
        // Small divergence → nudge.
        m.on_osc_song_time(20.5, 1.2);
        assert_eq!(drain_actions(&mut m), vec![EngineAction::NudgeBeats(20.5)]);
        // Large divergence (> 2s at 120 BPM = > 4 beats) → hard seek.
        m.on_osc_song_time(40.0, 1.3);
        assert_eq!(drain_actions(&mut m), vec![EngineAction::SeekBeats(40.0)]);
        assert_eq!(m.status(), TransportSyncStatus::DegradedOscOnly);
    }

    #[test]
    fn t11_osc_position_is_bookkeeping_while_clk_alive() {
        let mut m = settled_stopped_at_9();
        m.tick(1.0, true); // CLK alive
        m.on_osc_is_playing(true, 1.0);
        m.on_local_transport(true, 20.0, 1.1);
        drain_actions(&mut m);
        m.on_osc_song_time(40.0, 1.2);
        assert_eq!(
            drain_actions(&mut m),
            vec![],
            "CLK owns position; OSC song time must not move the engine"
        );
    }

    #[test]
    fn t12_tempo_updates_dead_reckoning_only() {
        let mut m = settled_stopped_at_9();
        m.on_osc_tempo(174.0, 1.0);
        assert_eq!(drain_actions(&mut m), vec![]);
        assert_eq!(drain_out(&mut m), vec![]);
    }

    #[test]
    fn t13_new_gesture_replaces_pending() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        drain_out(&mut m);
        // User changes their mind before the ack arrives.
        m.on_local_stop(1.1);
        assert_eq!(drain_out(&mut m), vec![OutMsg::StopPlaying]);
        // The old expectation is gone: a "playing at 128" observation now
        // CONTRADICTS the stop expectation and must not ack it.
        m.on_osc_is_playing(true, 1.2);
        m.on_osc_song_time(128.0, 1.2);
        assert!(m.sync_pending(), "abandoned expectation must not ack");
        // The stop's own ack settles it.
        m.on_osc_is_playing(false, 1.4);
        assert!(!m.sync_pending());
    }

    #[test]
    fn dead_reckoning_projects_observed_position_forward() {
        let mut m = AbletonTransportSync::new();
        m.on_osc_tempo(120.0, 0.0);
        m.on_osc_is_playing(true, 0.0);
        m.on_osc_song_time(10.0, 0.0);
        drain_actions(&mut m);
        // 2 beats/sec at 120 BPM: after 1s, observed ≈ 12. A play at 12
        // should value-match without commanding Ableton (D12).
        m.on_local_play(12.0, 1.0);
        assert_eq!(drain_out(&mut m), vec![]);
    }

    #[test]
    fn warning_clears_on_next_successful_ack() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        m.on_local_transport(true, 128.0, 1.0);
        drain_out(&mut m);
        let mut now = 1.0;
        for _ in 0..=MAX_RETRIES {
            now += 2.5;
            m.tick(now, true);
        }
        assert_eq!(m.status(), TransportSyncStatus::Warning);
        drain_actions(&mut m);
        m.on_local_transport(false, 128.0, now); // engine adopted the pause
        // Next gesture round-trips cleanly → Warning clears.
        m.on_local_play(128.0, now + 1.0);
        drain_out(&mut m);
        m.on_osc_is_playing(true, now + 1.1);
        m.on_osc_song_time(128.0, now + 1.1);
        assert_eq!(m.status(), TransportSyncStatus::Locked);
    }

    #[test]
    fn reset_drops_pending_state() {
        let mut m = settled_stopped_at_9();
        m.on_local_play(128.0, 1.0);
        assert!(m.sync_pending());
        m.reset();
        assert!(!m.sync_pending());
        assert_eq!(drain_out(&mut m), vec![]);
    }
}
