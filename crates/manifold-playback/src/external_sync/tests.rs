use super::*;

/// Seeded jitter in ±`amp` seconds.
struct Jitter(u64);

impl Jitter {
    fn next(&mut self, amp: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * amp
    }
}

/// Ticks at Live's tempo with ±1 ms of arrival jitter, and Link's beat at
/// each arrival integrated at Link's tempo. Returns (time, latched) per tick.
fn run_clock(guard: &mut LinkTempoGuard, until: f64, live_bpm: impl Fn(f64) -> f64, link_bpm: impl Fn(f64) -> f64) -> Vec<(f64, bool)> {
    let mut jitter = Jitter(0x9E37_79B9);
    let (mut t, mut link_beat, mut k) = (0.0, 0.0, 0i64);
    let mut out = Vec::new();
    while t < until {
        let period = 60.0 / (24.0 * live_bpm(t));
        let next = t + period;
        link_beat += period * link_bpm(t) / 60.0;
        t = next;
        k += 1;
        let arrival = t + jitter.next(0.001);
        // Link's beat at the arrival, not at the send.
        let at_arrival = link_beat + (arrival - t) * link_bpm(t) / 60.0;
        guard.observe_clock(arrival, arrival, k, at_arrival);
        out.push((t, guard.latched()));
    }
    out
}

#[test]
fn guard_latches_and_holds_through_tempo_changes() {
    let mut guard = LinkTempoGuard::new();
    let tempo = |t: f64| if t < 20.0 { 128.0 } else { 174.0 };
    let trace = run_clock(&mut guard, 40.0, tempo, tempo);
    let first = trace.iter().find(|(_, l)| *l).expect("latches").0;
    assert!(first <= GUARD_WINDOW + 0.1, "latched only at {first:.2} s");
    assert!(trace.iter().filter(|(t, _)| *t > first).all(|(_, l)| *l), "the 128 -> 174 jump kept the latch");
}

#[test]
fn guard_rejects_a_foreign_link_session() {
    let mut guard = LinkTempoGuard::new();
    let trace = run_clock(&mut guard, 30.0, |_| 128.0, |_| 127.0);
    assert!(trace.iter().all(|(_, l)| !*l), "a session at 127 against Live at 128 never latches");

    // Latched with Live on Link, then Link's session moves away: released
    // only once the disagreement has lasted 2 s.
    let mut guard = LinkTempoGuard::new();
    let trace = run_clock(&mut guard, 30.0, |_| 128.0, |t| if t < 15.0 { 128.0 } else { 126.0 });
    let released = trace.iter().find(|(t, l)| *t > 15.0 && !*l).expect("released").0;
    assert!(released > 15.0 + GUARD_UNLATCH_HOLD, "released at {released:.2} s, before the hold");
    assert!(released < 15.0 + GUARD_WINDOW + GUARD_UNLATCH_HOLD, "released late, at {released:.2} s");
}

#[test]
fn a_relocation_restarts_the_window_without_releasing() {
    let mut guard = LinkTempoGuard::new();
    let tempo = |_: f64| 120.0;
    run_clock(&mut guard, 10.0, tempo, tempo);
    assert!(guard.latched());
    // Song Position Pointer: the tick count jumps 96 beats ahead.
    let mut t = 10.0;
    let mut link_beat = 20.0;
    for k in 0..2_000i64 {
        t += 60.0 / (24.0 * 120.0);
        link_beat += 1.0 / 24.0;
        guard.observe_clock(t, t, 240 + 96 * 24 + k, link_beat);
        assert!(guard.latched(), "released at {t:.2} s after a relocation");
    }
}

#[test]
fn listener_guard_holds_two_seconds() {
    let mut guard = LinkTempoGuard::new();
    guard.observe_listener(0.0, 128.0, 128.1);
    assert!(guard.latched(), "0.08% apart latches");
    guard.observe_listener(1.0, 128.0, 129.0);
    guard.observe_listener(2.9, 128.0, 129.0);
    assert!(guard.latched(), "1.9 s of disagreement holds");
    guard.observe_listener(3.0, 128.0, 129.0);
    assert!(!guard.latched(), "2 s of disagreement releases");
    guard.observe_listener(3.1, 128.0, 128.0);
    assert!(guard.latched());
    guard.release();
    assert!(!guard.latched());
}

#[test]
fn link_start_rounds_the_offset_to_the_quantum() {
    let mut offset = LinkOffset::new();
    assert_eq!(offset.beat(37.3), None, "no sample before an offset");
    assert_eq!(offset.take(37.3, 10.0, 4.0), None, "nothing due");
    offset.request(OffsetTake::Quantum);
    assert_eq!(offset.take(37.3, 10.0, 4.0), Some(28.0));
    let beat = offset.beat(37.3).expect("taken");
    assert!(((37.3 - beat) % 4.0).abs() < 1e-9 && (beat - 10.0).abs() <= 2.0, "bar aligned, moved under half a quantum: {beat}");

    offset.request(OffsetTake::Exact);
    offset.take(41.0, 12.5, 4.0);
    assert_eq!(offset.beat(41.0), Some(12.5), "MANIFOLD's own start keeps its beat");
    offset.link_became_authority();
    assert!(!offset.is_due(), "Link returning after a dropout keeps the offset (D15)");
    offset.clear();
    assert_eq!((offset.get(), offset.is_due()), (None, false));
    offset.link_became_authority();
    assert!(offset.is_due(), "Link becoming the authority with no offset takes the exact one");
}

#[test]
fn live_tempo_follows_the_authority_and_the_guard() {
    let clk = Some((127.9, TempoPointSource::MidiClock));
    assert_eq!(live_tempo(ClockAuthority::Link, Some(128.0), None, false), Some((128.0, TempoPointSource::Link)));
    assert_eq!(live_tempo(ClockAuthority::MidiClock, Some(128.0), clk, false), clk);
    assert_eq!(live_tempo(ClockAuthority::MidiClock, Some(128.0), clk, true), Some((128.0, TempoPointSource::Link)));
    assert_eq!(live_tempo(ClockAuthority::MidiClock, None, clk, true), clk, "Link gone: CLK's own tempo, never the map's");
    assert_eq!(live_tempo(ClockAuthority::Osc, None, None, false), None, "timecode reports the map's");
    assert_eq!(live_tempo(ClockAuthority::Internal, Some(128.0), clk, true), None);
}
