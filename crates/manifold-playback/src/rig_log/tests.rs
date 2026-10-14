use super::*;
use std::collections::HashMap;
use std::time::Duration;

fn authority(s: &str) -> ClockAuthority {
    match s {
        "internal" => ClockAuthority::Internal,
        "link" => ClockAuthority::Link,
        "clk" => ClockAuthority::MidiClock,
        "osc" => ClockAuthority::Osc,
        other => panic!("authority {other}"),
    }
}

/// The inverse of `format_line`, kept beside the test so the line format is
/// pinned in Rust as well as in `scripts/sync_rig_report.py`.
fn parse_line(line: &str) -> RigRecord {
    let mut parts = line.trim_end().split(' ');
    let kind = parts.next().unwrap();
    let kv: HashMap<&str, &str> = parts.map(|p| p.split_once('=').unwrap()).collect();
    let u = |k: &str| kv[k].parse::<u64>().unwrap();
    let f = |k: &str| kv[k].parse::<f64>().unwrap();
    let f32_ = |k: &str| kv[k].parse::<f32>().unwrap();
    match kind {
        "clk" => RigRecord::ClkByte {
            at_ns: u("t"),
            midir_us: u("midir_us"),
            status: u8::from_str_radix(kv["st"], 16).unwrap(),
            d1: kv["d1"].parse().unwrap(),
            d2: kv["d2"].parse().unwrap(),
        },
        "osc_song" => RigRecord::OscSongTime { at_ns: u("t"), beats: f32_("beats") },
        "osc_tempo" => RigRecord::OscTempo { at_ns: u("t"), bpm: f32_("bpm") },
        "tick" => RigRecord::Tick {
            frame: u("f"),
            wake_ns: u("wake"),
            deadline_ns: u("dl"),
            frames: kv["n"].parse().unwrap(),
            index: kv["i"].parse().unwrap(),
            beat: f("b"),
            authority: authority(kv["a"]),
            playing: kv["p"] == "1",
            link_beat: f("lb"),
            link_bpm: f("lt"),
            link_peers: kv["lp"].parse().unwrap(),
        },
        "phase" => RigRecord::Phase {
            at_ns: u("t"),
            engine_beat: f("b"),
            link_beat: f("lb"),
            link_offset: f("lo"),
            clk_beat: f32_("cb"),
            clk_bpm: f32_("ct"),
            osc_beat: f32_("ob"),
            osc_age_s: f("oage"),
            osc_bpm: f32_("ot"),
        },
        "present" => RigRecord::PresentIssued { frame: u("f"), drawable_id: u("d"), at_ns: u("t") },
        "presented" => RigRecord::Presented { drawable_id: u("d"), presented_s: f("ps"), at_ns: u("t") },
        "authority" => RigRecord::Authority { at_ns: u("t"), from: authority(kv["from"]), to: authority(kv["to"]) },
        "transport" => RigRecord::Transport {
            at_ns: u("t"),
            source: match kv["src"] {
                "user" => TransportSource::User,
                "link" => TransportSource::Link,
                "clk" => TransportSource::MidiClock,
                "osc" => TransportSource::Osc,
                other => panic!("source {other}"),
            },
            kind: match kv["kind"] {
                "play" => TransportKind::Play,
                "pause" => TransportKind::Pause,
                "stop" => TransportKind::Stop,
                "seek" => TransportKind::Seek,
                "seek_beat" => TransportKind::SeekToBeat,
                "locate_clk" => TransportKind::LocateToClock,
                other => panic!("kind {other}"),
            },
            value: f("v"),
        },
        other => panic!("record kind {other}"),
    }
}

/// Field-wise equality where NaN equals NaN: compare the debug text, which
/// prints every float with full precision.
fn same(a: &RigRecord, b: &RigRecord) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

fn every_kind() -> Vec<RigRecord> {
    vec![
        RigRecord::ClkByte { at_ns: 1, midir_us: 123_456_789, status: 0xf8, d1: 0, d2: 0 },
        RigRecord::ClkByte { at_ns: 2, midir_us: 0, status: 0xf2, d1: 127, d2: 3 },
        RigRecord::OscSongTime { at_ns: 3, beats: 1_234.567_9 },
        RigRecord::OscTempo { at_ns: 4, bpm: 127.99 },
        RigRecord::Tick {
            frame: 5,
            wake_ns: 16_700_123,
            deadline_ns: 16_666_667,
            frames: 1,
            index: -3,
            beat: 0.1 + 0.2,
            authority: ClockAuthority::MidiClock,
            playing: true,
            link_beat: f64::NAN,
            link_bpm: f64::NAN,
            link_peers: 0,
        },
        RigRecord::Tick {
            frame: u64::MAX,
            wake_ns: u64::MAX,
            deadline_ns: 0,
            frames: u32::MAX,
            index: i64::MIN,
            beat: -1e-300,
            authority: ClockAuthority::Link,
            playing: false,
            link_beat: 1e9 + 0.123_456_789,
            link_bpm: 174.000_000_1,
            link_peers: 7,
        },
        RigRecord::Phase {
            at_ns: 6,
            engine_beat: 64.25,
            link_beat: 1000.5,
            link_offset: f64::NAN,
            clk_beat: 64.0,
            clk_bpm: 128.01,
            osc_beat: f32::NAN,
            osc_age_s: f64::INFINITY,
            osc_bpm: 0.0,
        },
        RigRecord::PresentIssued { frame: 7, drawable_id: 42, at_ns: 8 },
        RigRecord::Presented { drawable_id: 42, presented_s: 812_345.678_901_234, at_ns: 9 },
        RigRecord::Authority { at_ns: 10, from: ClockAuthority::Internal, to: ClockAuthority::Osc },
        RigRecord::Transport { at_ns: 11, source: TransportSource::User, kind: TransportKind::SeekToBeat, value: 32.5 },
        RigRecord::Transport { at_ns: 12, source: TransportSource::Link, kind: TransportKind::Play, value: f64::NAN },
        RigRecord::Transport { at_ns: 13, source: TransportSource::MidiClock, kind: TransportKind::Seek, value: 1.5 },
        RigRecord::Transport { at_ns: 14, source: TransportSource::Osc, kind: TransportKind::Pause, value: 0.0 },
        RigRecord::Transport { at_ns: 15, source: TransportSource::User, kind: TransportKind::LocateToClock, value: 2.0 },
        RigRecord::Transport { at_ns: 16, source: TransportSource::User, kind: TransportKind::Stop, value: 0.0 },
    ]
}

#[test]
fn records_round_trip_through_their_line() {
    let mut line = String::new();
    for record in every_kind() {
        line.clear();
        format_line(&record, &mut line);
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
        let back = parse_line(&line);
        assert!(same(&record, &back), "{record:?}\n{line}{back:?}");
    }
}

#[test]
fn emit_never_blocks_when_nobody_drains() {
    let (sink, _rx) = RigSink::new(64);
    let record = RigRecord::ClkByte { at_ns: 0, midir_us: 0, status: 0xf8, d1: 0, d2: 0 };
    let mut worst = Duration::ZERO;
    for _ in 0..20_000 {
        let start = Instant::now();
        sink.emit(record);
        worst = worst.max(start.elapsed());
    }
    assert_eq!(sink.dropped(), 20_000 - 64);
    // A blocked send would wait forever; a slow one would show here. One
    // scheduler hiccup on a loaded machine stays far under this.
    assert!(worst < Duration::from_millis(20), "worst emit {worst:?}");
}

#[test]
fn emit_after_the_writer_is_gone_counts_and_returns() {
    let (sink, rx) = RigSink::new(8);
    drop(rx);
    sink.emit(RigRecord::OscTempo { at_ns: 0, bpm: 120.0 });
    assert_eq!(sink.dropped(), 1);
}

/// The per-record cost a hot thread pays when the record is accepted: a
/// channel-sized burst into an empty channel, so every send succeeds.
#[test]
fn accepted_emit_cost() {
    let (sink, rx) = RigSink::new(CHANNEL_CAPACITY);
    let tick = every_kind()[4];
    let start = Instant::now();
    for _ in 0..CHANNEL_CAPACITY {
        sink.emit(tick);
    }
    let per = start.elapsed() / CHANNEL_CAPACITY as u32;
    assert_eq!(sink.dropped(), 0);
    assert_eq!(rx.try_iter().count(), CHANNEL_CAPACITY);
    eprintln!("rig_log emit: {per:?} per accepted record");
    assert!(per < Duration::from_micros(5), "emit {per:?}");
}

#[test]
fn emit_cost_with_a_live_writer() {
    let (sink, rx) = RigSink::new(CHANNEL_CAPACITY);
    let reader = std::thread::spawn(move || {
        let mut line = String::with_capacity(256);
        let mut n = 0u64;
        while let Ok(r) = rx.recv() {
            line.clear();
            format_line(&r, &mut line);
            n += 1;
        }
        n
    });
    let tick = every_kind()[4];
    const N: u32 = 200_000;
    let start = Instant::now();
    for _ in 0..N {
        sink.emit(tick);
    }
    let per = start.elapsed() / N;
    let dropped = sink.dropped();
    drop(sink);
    let received = reader.join().unwrap();
    assert_eq!(received + dropped, u64::from(N));
    eprintln!("rig_log emit: {per:?} per record flooding a live writer, {dropped} dropped of {N}");
    assert!(per < Duration::from_micros(5), "emit {per:?}");
}
