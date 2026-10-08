//! The live presentation cursor for `node.liquid_frame`
//! (`docs/GPU_FLIP_DISPLAY_HISTORY_DESIGN.md` section 3.4 (Cursor)). Live
//! uncoupled water presents a little behind the requested time, so a
//! finished snapshot lies on each side and the picture interpolates instead
//! of holding at the newest one. The lag is the observed retirement deficit
//! over the last two seconds of advancing transport, plus a quarter of the
//! newest publication gap.

use super::frame_history::{HistoryCore, Presentation};

const RATE: f64 = 0.05;
const FILL_RATE: f64 = 0.25;
const HORIZON: f64 = 0.5;
const WINDOW: f64 = 2.0;
const FILL: f64 = 1.0;
/// Buckets spanned by WINDOW; a sample stays in L for WINDOW to
/// WINDOW·(SPAN + 1)/SPAN of advancing transport at any frame rate.
const SPAN: u64 = 64;
const BUCKETS: usize = 72;
const BUCKET: f64 = WINDOW / SPAN as f64;
const EMPTY: (u64, f64) = (u64::MAX, 0.0);

/// One frame's clock inputs, as the domain's f32 scalars deliver them.
#[derive(Clone, Copy, Debug)]
pub struct CursorFrame {
    pub epoch: u32,
    /// The domain's `display_time`.
    pub requested: f64,
    /// Transport seconds.
    pub transport: f64,
    /// The domain's `dropped_seconds`; a rise marks a clock reanchor.
    pub dropped: f64,
}

/// What the last advancing step used, for traces and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorTrace {
    pub deficit: f64,
    pub guard: f64,
}

#[derive(Debug)]
pub struct DisplayCursor {
    c: Option<f64>,
    epoch: Option<u32>,
    r_prev: f64,
    t_prev: f64,
    d_prev: f64,
    /// Transport accumulated on frames whose requested time advanced.
    advancing: f64,
    fill_end: f64,
    window: [(u64, f64); BUCKETS],
    trace: CursorTrace,
}

impl Default for DisplayCursor {
    fn default() -> Self {
        Self {
            c: None,
            epoch: None,
            r_prev: 0.0,
            t_prev: 0.0,
            d_prev: 0.0,
            advancing: 0.0,
            fill_end: 0.0,
            window: [EMPTY; BUCKETS],
            trace: CursorTrace::default(),
        }
    }
}

impl DisplayCursor {
    /// The cursor, once the epoch has a retired endpoint.
    pub fn cursor(&self) -> Option<f64> {
        self.c
    }

    pub fn trace(&self) -> CursorTrace {
        self.trace
    }

    /// An exact frame: the next cursor frame starts over from the picture
    /// then shown.
    pub fn clear(&mut self) {
        self.c = None;
        self.epoch = None;
    }

    /// Advance the cursor, select at it and apply the cut rule. Call after
    /// this frame's retirement; returns the pin.
    pub fn present(&mut self, core: &mut HistoryCore, frame: CursorFrame) -> Option<Presentation> {
        if self.epoch != Some(frame.epoch) {
            self.epoch = Some(frame.epoch);
            self.c = None;
            self.window = [EMPTY; BUCKETS];
            self.r_prev = frame.requested;
            self.t_prev = frame.transport;
            self.d_prev = frame.dropped;
        }
        let r = frame.requested;
        let delta = (r - self.r_prev).max(0.0);
        let advanced = (frame.transport - self.t_prev).max(0.0);
        let reanchored = frame.dropped > self.d_prev;
        self.r_prev = r;
        self.t_prev = frame.transport;
        self.d_prev = frame.dropped;

        let Some(bounds) = core.retired_bounds() else {
            return core.pinned().copied();
        };
        let c = match self.c {
            None => {
                // The epoch's first retirement, or a switch from exact: start
                // at the picture already shown when it is comparable.
                let shown = core.current_pin().map_or(bounds.earliest, |p| p.presented_time());
                self.fill_end = self.advancing + FILL;
                shown.max(bounds.earliest).min(bounds.newest)
            }
            Some(_) if delta == 0.0 && core.pinned().is_some() => return core.pinned().copied(),
            Some(c) => {
                debug_assert!(c <= bounds.newest, "the cursor never passes N: {c} > {}", bounds.newest);
                // A reanchor's jump is transport the clock dropped, not
                // retirement delay: it neither ages nor feeds the window.
                if delta > 0.0 && !reanchored {
                    self.advancing += advanced;
                    self.record((r - bounds.newest).max(0.0));
                }
                let deficit = self.deficit();
                let guard = bounds.second.map_or(0.0, |second| (bounds.newest - second) / 4.0);
                self.trace = CursorTrace { deficit, guard };
                let target = r - deficit - guard;
                let base = c + delta;
                let k = if self.advancing < self.fill_end { FILL_RATE } else { RATE };
                let u = ((target - base) / HORIZON).clamp(-k, k);
                (base + delta * u).min(bounds.newest)
            }
        };
        self.c = Some(c);
        let shown = core.select(c);
        if let Some(pin) = core.current_pin()
            && pin.t_a > c
        {
            // A run or generation cut: the only jump, always forward.
            self.c = Some(pin.t_a);
        }
        shown
    }

    fn index(&self) -> u64 {
        (self.advancing / BUCKET) as u64
    }

    fn record(&mut self, behind: f64) {
        let index = self.index();
        let bucket = &mut self.window[(index % BUCKETS as u64) as usize];
        if bucket.0 == index {
            bucket.1 = bucket.1.max(behind);
        } else {
            *bucket = (index, behind);
        }
    }

    fn deficit(&self) -> f64 {
        let index = self.index();
        self.window
            .iter()
            .filter(|(i, _)| *i <= index && index - *i <= SPAN)
            .map(|&(_, behind)| behind)
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::water::liquid::frame_history::{Layout, FIELDS};
    use manifold_physics::clock::{ClockFrame, SimulationClock};

    const LATTICE: [u32; 7] = [0, 0, 0, 1, 8, 8, 8];

    fn layout(epoch: u32) -> Layout {
        Layout { epoch, lattice: LATTICE, fields: [0; FIELDS] }
    }

    #[derive(Clone, Debug)]
    struct Shown {
        frame: ClockFrame,
        pin: Option<Presentation>,
        c: Option<f64>,
        newest: Option<f64>,
        trace: CursorTrace,
        published: bool,
    }

    impl Shown {
        fn lag(&self) -> f64 {
            f64::from(self.frame.display_time as f32) - self.c.expect("cursor")
        }
    }

    /// `liquid_frame`'s live order over a [`HistoryCore`] and the real
    /// clock. A publication stamped at frame s retires `delay(s)` frames
    /// later, in publication order.
    struct Rig {
        core: HistoryCore,
        cursor: DisplayCursor,
        clock: SimulationClock,
        frame: u64,
        delay: Box<dyn Fn(u64) -> u64>,
        layout: Layout,
        identity: u32,
        cursor_on: bool,
        offline: bool,
        speed: f32,
        reset: f32,
        hz: f64,
        transport: f64,
        /// The previous cursor frame's epoch and c, for the step invariants.
        last: Option<(u32, f64)>,
    }

    impl Rig {
        fn new(delay: u64) -> Self {
            Self::with_delay(Box::new(move |_| delay))
        }

        fn with_delay(delay: Box<dyn Fn(u64) -> u64>) -> Self {
            Self {
                core: HistoryCore::default(),
                cursor: DisplayCursor::default(),
                clock: SimulationClock::default(),
                frame: 0,
                delay,
                layout: layout(1),
                identity: 1,
                cursor_on: true,
                offline: false,
                speed: 1.0,
                reset: 0.0,
                hz: 30.0,
                transport: 0.0,
                last: None,
            }
        }

        /// One frame at transport `t`.
        fn at(&mut self, t: f64) -> Shown {
            self.transport = t;
            let frame = self.clock.advance(t, 1.0 / self.hz, self.speed, self.reset, false, self.offline);
            self.frame += 1;
            let now = self.frame;
            let delay = &self.delay;
            let complete = |stamp: u64| stamp == 0 || stamp + delay(stamp) <= now;
            let layout = Layout { epoch: frame.epoch, ..self.layout };
            self.core.set_layout(layout);
            let identity = self.identity;
            self.core.retire(complete, |_| Some([10, identity, 1, 0]));
            // The production f32 scalars.
            let requested = f64::from(frame.display_time as f32);
            let simulation = f64::from(frame.simulation_time as f32);
            let pin = if self.cursor_on && !self.offline {
                self.cursor.present(&mut self.core, CursorFrame {
                    epoch: frame.epoch,
                    requested,
                    transport: t,
                    dropped: f64::from(frame.dropped_seconds as f32),
                })
            } else {
                self.cursor.clear();
                self.core.select(requested)
            };
            self.core.stamp_readers(now);
            let complete = |stamp: u64| stamp == 0 || stamp + delay(stamp) <= now;
            self.core.reclaim(complete);
            let mut published = false;
            if self.core.wants_publication(simulation) {
                match self.core.free_slot(|_| true).or_else(|| self.core.can_grow().then(|| self.core.push_free())) {
                    Some(slot) => {
                        self.core.begin(slot, simulation, now);
                        published = true;
                        if self.offline {
                            self.core.release_writer(slot);
                        }
                    }
                    None => self.core.skip(simulation),
                }
            }
            let pin = if self.offline {
                self.core.retire(|stamp| stamp == 0, |_| Some([10, identity, 1, 0]));
                self.core.select(requested)
            } else {
                pin
            };
            // Every cursor step: c ≤ N, and c never decreases within an
            // epoch. The exceptions are explicit: a new epoch, and leaving
            // cursor mode (which clears `last`).
            let cursor_mode = self.cursor_on && !self.offline;
            match (cursor_mode, self.cursor.cursor()) {
                (true, Some(c)) => {
                    // With nothing retired in the generation (step 4) c waits.
                    if let Some(b) = self.core.retired_bounds() {
                        assert!(c <= b.newest, "c {c} passed N {}", b.newest);
                    }
                    if let Some((epoch, previous)) = self.last
                        && epoch == frame.epoch
                    {
                        assert!(c >= previous, "c stepped back {previous} -> {c}");
                    }
                    self.last = Some((frame.epoch, c));
                }
                (true, None) => {}
                (false, _) => self.last = None,
            }
            if let (Some(p), Some(b)) = (pin, self.core.retired_bounds()) {
                assert!(p.presented_time() <= p.t_b);
                assert!((0.0..=1.0).contains(&p.blend));
                if self.cursor_on && !self.offline && self.core.current_pin() == Some(p) {
                    assert!(p.t_b <= b.newest);
                }
            }
            Shown {
                frame,
                pin,
                c: if self.cursor_on && !self.offline { self.cursor.cursor() } else { Some(requested) },
                newest: self.core.retired_bounds().map(|b| b.newest),
                trace: self.cursor.trace(),
                published,
            }
        }

        /// `frames` frames at `fps` from the current transport.
        fn run(&mut self, fps: f64, frames: usize) -> Vec<Shown> {
            let start = self.transport;
            (1..=frames).map(|i| self.at(start + i as f64 / fps)).collect()
        }
    }

    /// c never decreases within an epoch.
    fn assert_monotone(frames: &[Shown]) {
        for pair in frames.windows(2) {
            if let (Some(a), Some(b)) = (pair[0].c, pair[1].c)
                && pair[0].frame.epoch == pair[1].frame.epoch
            {
                assert!(b >= a, "the cursor stepped back: {a} -> {b}");
            }
        }
    }

    fn median(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    }

    /// Frames whose picture did not move although the request did.
    fn holds(frames: &[Shown]) -> usize {
        frames
            .windows(2)
            .filter(|w| {
                let (a, b) = (w[0].pin.map(|p| p.presented_time()), w[1].pin.map(|p| p.presented_time()));
                w[1].frame.display_time > w[0].frame.display_time && a.is_some() && a == b
            })
            .count()
    }

    #[test]
    fn cursor_equilibrium_matches_the_latency_table() {
        // Section 3.5's r − c column.
        for (fps, hz, delay, speed, expected) in [
            (60.0, 30.0, 3, 1.0, 0.042),
            (60.0, 30.0, 4, 1.0, 0.058),
            (30.0, 30.0, 3, 1.0, 0.075),
            (30.0, 30.0, 4, 1.0, 0.108),
            (60.0, 24.0, 3, 1.0, 0.052),
            (27.0, 30.0, 3, 1.0, 0.117),
            (60.0, 30.0, 3, 0.5, 0.021),
        ] {
            let mut rig = Rig::new(delay);
            rig.hz = hz;
            rig.speed = speed;
            let frames = rig.run(fps, (12.0 * fps) as usize);
            assert_monotone(&frames);
            let settled = &frames[(3.0 * fps) as usize..];
            let lag = median(settled.iter().map(Shown::lag).collect());
            assert!((lag - expected).abs() <= 0.002, "{fps}/{hz} R{delay} speed {speed}: r − c {lag:.4}, table {expected}");
            assert_eq!(holds(settled), 0, "{fps}/{hz} R{delay}: no hold after warm-up");
            let interior = settled.iter().filter(|s| s.pin.is_some_and(|p| p.blend > 0.0 && p.blend < 1.0)).count();
            assert!(interior * 10 >= settled.len() * 9, "{fps}/{hz} R{delay}: blend interior on {interior}/{}", settled.len());
        }
    }

    #[test]
    fn cursor_retire_stall_holds_at_newest_then_recovers() {
        // R 3 → 6 for publications stamped in frames 361..=420 (seconds 6–7).
        let mut rig = Rig::with_delay(Box::new(|stamp| if (361..=420).contains(&stamp) { 6 } else { 3 }));
        let frames = rig.run(60.0, 900);
        assert_monotone(&frames);
        let before = median(frames[300..360].iter().map(Shown::lag).collect());
        for (i, s) in frames.iter().enumerate().skip(361).take(120) {
            if i > 0 && holds(&frames[i - 1..=i]) == 1 {
                assert_eq!(s.c, s.newest, "every hold is at N");
            }
        }
        // The stall ends when the last slow publication retires (frame 426);
        // 4 s of advancing transport later the lag is back within 10%.
        let after = &frames[426 + 240..426 + 300];
        for s in after {
            assert!((s.lag() - before).abs() <= 0.1 * before, "recovered: {:.4} vs {before:.4}", s.lag());
        }
    }

    #[test]
    fn cursor_jitter_never_steps_backwards() {
        let mut seed = 0x9e37_79b9_u64;
        let delays: Vec<u64> = (0..2000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                1 + seed % 4
            })
            .collect();
        // Publication order is retirement order: a slow one holds the rest.
        let mut rig = Rig::with_delay(Box::new(move |stamp| delays[stamp as usize % delays.len()]));
        let mut t = 0.0;
        let mut frames = Vec::new();
        for i in 0..1200u32 {
            t += if i % 7 == 3 { 1.6 } else { 0.9 } / 60.0;
            frames.push(rig.at(t));
        }
        assert_monotone(&frames);
    }

    #[test]
    fn cursor_reanchor_catches_up_without_poisoning_the_window() {
        for jump in [10.0, 0.5] {
            let mut rig = Rig::new(3);
            let frames = rig.run(60.0, 360);
            let before = median(frames[240..].iter().map(Shown::lag).collect());
            let deficit = frames.last().expect("frames").trace.deficit;
            let retired_before = rig.core.retired_bounds().expect("retired").newest;
            let seek = rig.at(rig.transport + jump);
            assert!(seek.frame.reanchored, "a jump of {jump} s reanchors");
            assert_eq!(seek.c, seek.newest, "the seek frame catches up to N");
            let pin = seek.pin.expect("shown");
            assert!(pin.t_b <= retired_before, "the jump shows an already finished snapshot");
            assert!(seek.published as u32 <= 1, "no extra publication");
            assert!((seek.trace.deficit - deficit).abs() < 1e-9, "the reanchor frame records no deficit");
            // The jump leaves c about one Sim Rate interval further behind
            // than equilibrium; it closes with time constant HORIZON.
            let after = rig.run(60.0, 150);
            assert_monotone(&after);
            let settled = median(after[120..].iter().map(Shown::lag).collect());
            assert!((settled - before).abs() <= 0.1 * before, "jump {jump}: lag {settled:.4} vs {before:.4}");
        }
    }

    #[test]
    fn cursor_epoch_start_holds_at_most_two() {
        for backward in [false, true] {
            let mut rig = Rig::new(3);
            rig.run(60.0, 600);
            let old = rig.core.pinned().copied().expect("shown");
            if backward {
                rig.transport = 2.0;
            } else {
                rig.reset = 1.0;
            }
            let frames = rig.run(60.0, 60);
            let first = frames.iter().position(|s| s.c.is_some()).expect("the new epoch retires");
            for s in &frames[..first] {
                assert_eq!(s.pin, Some(old), "the old water shows until the first retirement");
            }
            assert_eq!(frames[first].c, Some(0.0), "the cursor starts at the earliest endpoint");
            assert_monotone(&frames[first..]);
            assert!(holds(&frames[first..]) <= 2, "backward {backward}: {} holds", holds(&frames[first..]));
        }
    }

    #[test]
    fn cursor_speed_zero_then_resume() {
        for delay in [3, 4] {
            let mut rig = Rig::new(delay);
            rig.run(60.0, 240);
            rig.speed = 0.0;
            let stopped = rig.run(60.0, 300);
            assert_monotone(&stopped);
            // One Sim Rate interval (two frames) of rising requests, then still.
            let still = &stopped[4..];
            for s in still {
                assert_eq!(s.pin, still[0].pin, "R{delay}: the stopped picture holds");
            }
            rig.speed = 1.0;
            let resumed = rig.run(60.0, 120);
            assert_monotone(&resumed);
            assert!(holds(&resumed) <= 1, "R{delay}: {} holds after resuming", holds(&resumed));
        }
    }

    #[test]
    fn cursor_pause_longer_than_the_window_keeps_it() {
        let mut rig = Rig::new(3);
        rig.run(60.0, 300);
        let deficit = rig.cursor.trace().deficit;
        let c = rig.cursor.cursor();
        let pin = rig.core.pinned().copied();
        let transport = rig.transport;
        for _ in 0..300 {
            let s = rig.at(transport);
            assert_eq!((s.c, s.pin), (c, pin), "paused frames hold");
        }
        let resumed = rig.at(transport + 1.0 / 60.0);
        assert_eq!(resumed.trace.deficit, deficit, "a 5 s pause does not age the window");
    }

    #[test]
    fn cursor_pause_holds_through_late_cuts() {
        // A run cut and a generation cut retiring while paused.
        for generation in [false, true] {
            let mut rig = Rig::new(3);
            rig.run(60.0, 300);
            if generation {
                rig.layout.fields[1] = 64;
            } else {
                rig.identity = 2;
            }
            // One more ticking frame publishes into the new run or generation.
            let t = rig.transport + 2.0 / 60.0;
            rig.at(t);
            let held = rig.core.pinned().copied();
            for _ in 0..10 {
                let s = rig.at(t);
                assert_eq!(s.pin, held, "generation {generation}: a late cut does not move a paused picture");
            }
        }
        // A reset while paused shows the new epoch when it retires.
        let mut rig = Rig::new(3);
        rig.run(60.0, 300);
        let t = rig.transport;
        let old = rig.core.pinned().copied();
        rig.reset = 1.0;
        let frames: Vec<Shown> = (0..10).map(|_| rig.at(t)).collect();
        assert!(frames.iter().any(|s| s.pin != old), "the reset is shown while paused");
    }

    #[test]
    fn cursor_mode_flip_never_steps_back() {
        let mut rig = Rig::new(3);
        rig.cursor_on = false;
        rig.run(60.0, 300);
        let exact = rig.core.pinned().copied().expect("shown").presented_time();
        rig.cursor_on = true;
        let flipped = rig.run(60.0, 120);
        assert!(flipped[0].c.expect("cursor") >= exact, "exact → cursor starts at the shown picture");
        assert_monotone(&flipped);
        rig.cursor_on = false;
        let back = rig.at(rig.transport + 1.0 / 60.0);
        // Cursor → exact presents r at once: the pin brackets r, or holds
        // the newest endpoint whole when r is past it.
        let r = f64::from(back.frame.display_time as f32);
        let pin = back.pin.expect("shown");
        let newest = back.newest.expect("retired");
        if r >= newest {
            assert_eq!((pin.t_b, pin.blend), (newest, 1.0), "past N the exact pin is N whole");
        } else {
            assert!(pin.t_a <= r && r < pin.t_b, "the exact pin brackets r: {} {r} {}", pin.t_a, pin.t_b);
            assert!((pin.presented_time() - r).abs() < 1e-6, "the exact pin presents r");
        }
    }

    #[test]
    fn cursor_speed_change_rescales() {
        let mut rig = Rig::new(3);
        rig.run(60.0, 300);
        for speed in [2.0, 0.5] {
            rig.speed = speed;
            let frames = rig.run(60.0, 360);
            assert_monotone(&frames);
            let settled = &frames[240..];
            // Per frame, after settling: the advance stays within ±RATE of Δ.
            for pair in settled.windows(2) {
                let delta = f64::from(pair[1].frame.display_time as f32) - f64::from(pair[0].frame.display_time as f32);
                let step = pair[1].c.expect("c") - pair[0].c.expect("c");
                assert!((step - delta).abs() <= RATE * delta + 1e-6, "speed {speed}: step {step} for Δ {delta}");
            }
            let advanced = settled.last().and_then(|s| s.c).expect("c") - settled[0].c.expect("c");
            let requested = f64::from(settled.last().expect("frame").frame.display_time as f32) - f64::from(settled[0].frame.display_time as f32);
            assert!((advanced / requested - 1.0).abs() <= 0.05, "speed {speed}: {advanced} vs {requested}");
        }
    }

    #[test]
    fn cursor_f32_at_one_hour() {
        // An hour of live play, so r and N carry an hour's f32 rounding.
        let mut rig = Rig::new(3);
        for i in 1..=3600 * 60 {
            rig.at(f64::from(i) / 60.0);
        }
        let frames = rig.run(60.0, 600);
        assert!(frames[0].frame.simulation_time > 3500.0);
        assert_monotone(&frames);
        let lag = median(frames[180..].iter().map(Shown::lag).collect());
        assert!((lag - 0.042).abs() <= 0.003, "r − c at one hour {lag:.4}");
    }

    #[test]
    fn cursor_exact_modes_present_the_request() {
        // Unwired / coupled (cursor off) and offline with the wire at 1 select
        // exactly as P1 does at r.
        for offline in [false, true] {
            let mut rig = Rig::new(3);
            rig.cursor_on = offline;
            rig.offline = offline;
            let frames = rig.run(60.0, 240);
            for s in &frames {
                let r = f64::from(s.frame.display_time as f32);
                assert_eq!(s.c, Some(r));
            }
        }
        // Offline with the cursor requested: the same pictures as without it.
        let mut on = Rig::new(3);
        on.offline = true;
        on.cursor_on = true;
        let mut off = Rig::new(3);
        off.offline = true;
        off.cursor_on = false;
        let (a, b) = (on.run(60.0, 240), off.run(60.0, 240));
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.pin, y.pin, "export ignores display_cursor");
        }
    }

    #[test]
    fn cursor_held_frame_reemits_the_pin() {
        let mut rig = Rig::new(3);
        rig.run(60.0, 300);
        let advancing = rig.cursor.advancing;
        let t = rig.transport;
        let first = rig.at(t);
        // Repeated frames: retirements may land, the picture holds.
        for _ in 0..4 {
            let again = rig.at(t);
            assert_eq!(again.pin, first.pin);
            assert_eq!(again.c, first.c);
        }
        assert_eq!(rig.cursor.advancing, advancing, "held frames do not age the window");
    }
}
