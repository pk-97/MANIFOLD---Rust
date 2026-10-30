//! Decisions around the external sources that the content thread used to
//! hold inline (FRAME_EXACT_TRANSPORT_DESIGN D15, D21): the Link beat
//! offset, the guard that lets Link's tempo stand in for MIDI clock's and
//! AbletonOSC's, and the live tempo each authority reports.
//!
//! Pure: seconds on the caller's monotonic origin, no allocation after
//! construction.

use manifold_core::types::{ClockAuthority, TempoPointSource};

/// Link and the other source agree when their tempos are this close (D21).
pub const GUARD_TOLERANCE: f64 = 0.002;
/// s a disagreement must last before the guard lets go.
pub const GUARD_UNLATCH_HOLD: f64 = 2.0;
/// s of tick arrivals the MIDI clock comparison spans.
pub const GUARD_WINDOW: f64 = 4.0;
const TICKS_PER_BEAT: f64 = 24.0;
/// One entry per landed tick: 4 s at 300 BPM is 480 ticks.
const WINDOW_RING: usize = 512;

/// How a due Link offset is taken (D15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetTake {
    /// MANIFOLD chose its position (its own Play or seek, or Link becoming
    /// the authority while it plays): the beat does not move.
    Exact,
    /// Link started the play: MANIFOLD's bar lines up with Link's.
    Quantum,
}

/// MANIFOLD's beat under Link is `link_beat(R) − offset` (D15).
#[derive(Debug, Clone, Default)]
pub struct LinkOffset {
    offset: Option<f64>,
    due: Option<OffsetTake>,
}

impl LinkOffset {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<f64> {
        self.offset
    }

    /// Stop and project load: Link supplies no sample until the next start.
    pub fn clear(&mut self) {
        self.offset = None;
        self.due = None;
    }

    /// A start or seek: the offset is taken at the next read instant.
    pub fn request(&mut self, take: OffsetTake) {
        self.due = Some(take);
    }

    /// Link became the authority while MANIFOLD plays. With no offset yet,
    /// the exact one, so the beat does not move. Returning after a dropout,
    /// the offset is kept: the follower trims or seeks the gap's drift back,
    /// so MANIFOLD's bar returns to Link's (D15).
    pub fn link_became_authority(&mut self) {
        if self.offset.is_none() {
            self.request(OffsetTake::Exact);
        }
    }

    pub fn is_due(&self) -> bool {
        self.due.is_some()
    }

    /// Takes a due offset from Link's beat at the read instant and the beat
    /// of the frame this tick shows. A Link start rounds it to whole
    /// quanta, so the start moves by at most half a quantum.
    pub fn take(&mut self, link_beat: f64, frame_beat: f64, quantum: f64) -> Option<f64> {
        if !(link_beat.is_finite() && frame_beat.is_finite()) {
            return None;
        }
        let take = self.due.take()?;
        let exact = link_beat - frame_beat;
        let offset = match take {
            OffsetTake::Quantum if quantum > 0.0 => (exact / quantum).round() * quantum,
            _ => exact,
        };
        self.offset = Some(offset);
        Some(offset)
    }

    /// Link's beat as MANIFOLD's, once an offset is taken.
    pub fn beat(&self, link_beat: f64) -> Option<f64> {
        self.offset.map(|o| link_beat - o).filter(|b| b.is_finite())
    }
}

/// Whether Link's session tempo may stand in for MIDI clock's or
/// AbletonOSC's (D21). A latch: it holds through tempo changes and lets go
/// only after a disagreement over 0.2% has lasted 2 s.
#[derive(Debug, Clone)]
pub struct LinkTempoGuard {
    latched: bool,
    disagree_since: Option<f64>,
    /// (arrival, tick, Link's beat at the arrival), oldest first from `head`.
    window: [(f64, i64, f64); WINDOW_RING],
    head: usize,
    len: usize,
}

impl Default for LinkTempoGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkTempoGuard {
    pub fn new() -> Self {
        Self {
            latched: false,
            disagree_since: None,
            window: [(0.0, 0, 0.0); WINDOW_RING],
            head: 0,
            len: 0,
        }
    }

    pub fn latched(&self) -> bool {
        self.latched
    }

    /// No Link session, or no source to compare: the guard lets go at once.
    pub fn release(&mut self) {
        if self.latched {
            log::info!("[Transport] guard: Link tempo released, nothing to compare");
        }
        self.latched = false;
        self.disagree_since = None;
        self.len = 0;
    }

    /// MIDI clock: tick `tick` arrived at `arrival` (the window's clock),
    /// when Link's beat was `link_beat`. Compares Link's beat advance with
    /// the ticks counted over the last 4 s of arrivals; `now` times the hold.
    pub fn observe_clock(&mut self, now: f64, arrival: f64, tick: i64, link_beat: f64) {
        if !(arrival.is_finite() && link_beat.is_finite() && now.is_finite()) {
            return;
        }
        if let Some((at, k, beat)) = self.newest() {
            if tick == k {
                return;
            }
            // A relocation on either side restarts the window.
            let ratio = (link_beat - beat) / ((tick - k) as f64 / TICKS_PER_BEAT);
            if tick < k || arrival < at || !(0.5..=2.0).contains(&ratio) {
                self.len = 0;
            }
        }
        self.push((arrival, tick, link_beat));
        while self.len >= 2 && self.entry(1).0 <= arrival - GUARD_WINDOW {
            self.head = (self.head + 1) % WINDOW_RING;
            self.len -= 1;
        }
        let (at0, k0, beat0) = self.entry(0);
        if arrival - at0 >= GUARD_WINDOW {
            let ticks = (tick - k0) as f64 / TICKS_PER_BEAT;
            self.judge(now, ((link_beat - beat0) / ticks - 1.0).abs(), "MIDI clock");
        }
    }

    /// AbletonOSC: Link's session tempo against the tempo listener's.
    pub fn observe_listener(&mut self, now: f64, link_bpm: f64, listener_bpm: f64) {
        if link_bpm > 0.0 && listener_bpm > 0.0 && now.is_finite() {
            self.judge(
                now,
                (link_bpm / listener_bpm - 1.0).abs(),
                "AbletonOSC tempo",
            );
        }
    }

    fn judge(&mut self, now: f64, gap: f64, what: &str) {
        if gap <= GUARD_TOLERANCE {
            if !self.latched {
                log::info!(
                    "[Transport] guard: Link tempo latched, {what} within {:.3}%",
                    gap * 100.0
                );
            }
            self.latched = true;
            self.disagree_since = None;
        } else if self.latched {
            let since = *self.disagree_since.get_or_insert(now);
            if now - since >= GUARD_UNLATCH_HOLD {
                log::info!(
                    "[Transport] guard: Link tempo released, {what} {:.3}% apart for 2 s",
                    gap * 100.0
                );
                self.latched = false;
                self.disagree_since = None;
            }
        }
    }

    fn entry(&self, i: usize) -> (f64, i64, f64) {
        self.window[(self.head + i) % WINDOW_RING]
    }

    fn newest(&self) -> Option<(f64, i64, f64)> {
        (self.len > 0).then(|| self.entry(self.len - 1))
    }

    fn push(&mut self, e: (f64, i64, f64)) {
        if self.len == WINDOW_RING {
            self.head = (self.head + 1) % WINDOW_RING;
            self.len -= 1;
        }
        self.window[(self.head + self.len) % WINDOW_RING] = e;
        self.len += 1;
    }
}

/// The live tempo an authority reports (D21): its base tempo, never the
/// trimmed rate. Link's session tempo under Link; under MIDI clock and
/// AbletonOSC, Link's while the guard is latched, else the source's own
/// (`own`); under Audio, the follower's own tempo (`own`); none under
/// timecode, whose tempo is the map's, or Internal.
pub fn live_tempo(
    authority: ClockAuthority,
    link_bpm: Option<f64>,
    own: Option<(f64, TempoPointSource)>,
    latched: bool,
) -> Option<(f64, TempoPointSource)> {
    let link = link_bpm.map(|b| (b, TempoPointSource::Link));
    match authority {
        ClockAuthority::Link => link,
        ClockAuthority::MidiClock | ClockAuthority::Osc => {
            if latched {
                link.or(own)
            } else {
                own
            }
        }
        ClockAuthority::Audio => own,
        ClockAuthority::Internal => None,
    }
}

#[cfg(test)]
mod tests;
