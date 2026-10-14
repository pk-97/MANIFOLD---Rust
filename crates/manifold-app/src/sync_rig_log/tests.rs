use super::*;
use manifold_playback::rig_log::{RigSink, TransportKind, TransportSource};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manifold-rig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    files.sort();
    files
}

fn sample_records() -> Vec<RigRecord> {
    vec![
        RigRecord::ClkByte { at_ns: 1, midir_us: 99, status: 0xf8, d1: 0, d2: 0 },
        RigRecord::OscSongTime { at_ns: 2, beats: 16.5 },
        RigRecord::Tick {
            frame: 3,
            wake_ns: 4,
            deadline_ns: 5,
            frames: 1,
            index: 6,
            beat: 0.25,
            authority: ClockAuthority::Link,
            playing: true,
            link_beat: 100.25,
            link_bpm: 128.0,
            link_peers: 1,
        },
        RigRecord::PresentIssued { frame: 3, drawable_id: 7, at_ns: 8 },
        RigRecord::Presented { drawable_id: 7, presented_s: 1234.5, at_ns: 9 },
        RigRecord::Transport {
            at_ns: 10,
            source: TransportSource::User,
            kind: TransportKind::SeekToBeat,
            value: 64.0,
        },
    ]
}

#[test]
fn records_reach_the_file_in_order() {
    let dir = temp_dir("roundtrip");
    let writer = RigWriter::open(dir.clone(), "sync-test".into(), PART_BYTES, PARTS_KEPT).unwrap();
    let (sink, rx) = RigSink::new(64);
    let thread = std::thread::spawn(move || run_with(rx, writer, false));
    let records = sample_records();
    for r in &records {
        sink.emit(*r);
    }
    drop(sink);
    thread.join().unwrap();

    let files = log_files(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.starts_with("# manifold sync rig log v1 part=0 "), "{text}");
    let body: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("status "))
        .collect();
    let mut expected = String::new();
    for r in &records {
        rig_log::format_line(r, &mut expected);
    }
    assert_eq!(body, expected.lines().collect::<Vec<_>>());
    assert!(text.lines().any(|l| l.starts_with("status ") && l.contains(" dropped=")), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parts_rotate_and_old_parts_are_pruned() {
    let dir = temp_dir("rotate");
    std::fs::write(dir.join("sync-0000000000000-1-p0000.log"), "old session\n").unwrap();
    std::fs::write(dir.join("session-keep.log"), "not ours\n").unwrap();
    let mut writer = RigWriter::open(dir.clone(), "sync-9999999999999-2".into(), 400, 3).unwrap();
    let record = sample_records()[2];
    for _ in 0..200 {
        writer.record(&record).unwrap();
    }
    writer.flush().unwrap();

    let files = log_files(&dir);
    let ours: Vec<_> = files
        .iter()
        .filter_map(|p| p.file_name()?.to_str())
        .filter(|n| n.starts_with(FILE_PREFIX))
        .collect();
    assert_eq!(ours.len(), 3, "{ours:?}");
    assert!(ours.iter().all(|n| n.starts_with("sync-9999999999999-2-p")), "{ours:?}");
    assert_eq!(writer.path().file_name().unwrap().to_str().unwrap(), *ours.last().unwrap());
    assert!(dir.join("session-keep.log").exists());
    for name in &ours {
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(text.starts_with("# manifold sync rig log v1 part="), "{name}: {text}");
        assert!(text.len() < 400 + 300, "{name} is {} bytes", text.len());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn facts(wake: Instant, authority: ClockAuthority) -> TickFacts {
    TickFacts {
        frame: 1,
        wake,
        deadline: None,
        frames: 1,
        index: 0,
        beat: 0.0,
        authority,
        playing: false,
        link: None,
        link_offset: 0.0,
        clk: Some((4.0, 128.0)),
        osc: None,
    }
}

#[test]
fn probe_emits_tick_every_frame_phase_every_five_seconds_and_authority_on_change() {
    let mut probe = TickProbe::default();
    let mut out = Vec::new();
    let t0 = rig_log::epoch() + Duration::from_secs(1);
    probe.after_tick_into(&facts(t0, ClockAuthority::Internal), |r| out.push(r));
    probe.after_tick_into(&facts(t0 + Duration::from_secs(1), ClockAuthority::Internal), |r| out.push(r));
    probe.after_tick_into(&facts(t0 + Duration::from_secs(2), ClockAuthority::MidiClock), |r| out.push(r));
    probe.after_tick_into(&facts(t0 + Duration::from_secs(6), ClockAuthority::MidiClock), |r| out.push(r));

    let kinds: Vec<&str> = out
        .iter()
        .map(|r| match r {
            RigRecord::Authority { .. } => "authority",
            RigRecord::Tick { .. } => "tick",
            RigRecord::Phase { .. } => "phase",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["authority", "tick", "phase", "tick", "authority", "tick", "tick", "phase"]
    );
    assert!(matches!(
        out[4],
        RigRecord::Authority { from: ClockAuthority::Internal, to: ClockAuthority::MidiClock, .. }
    ));
    let RigRecord::Phase { clk_beat, osc_beat, link_beat, .. } = out[2] else { unreachable!() };
    assert_eq!(clk_beat, 4.0);
    assert!(osc_beat.is_nan() && link_beat.is_nan());
}

/// A 256-byte EDID: base block plus a CTA-861 extension whose HDMI vendor
/// block says video latency byte 11 (20 ms) and audio unknown.
fn synthetic_edid(latency_flags: u8) -> Vec<u8> {
    let mut edid = vec![0u8; 256];
    edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
    // "SAM": S=19, A=1, M=13 packed as five-bit letters.
    edid[8..10].copy_from_slice(&((19u16 << 10) | (1 << 5) | 13).to_be_bytes());
    edid[10..12].copy_from_slice(&3021u16.to_le_bytes());
    edid[126] = 1;
    let ext = &mut edid[128..];
    ext[0] = 0x02;
    ext[1] = 0x03;
    ext[2] = 4 + 11;
    ext[4] = (3 << 5) | 10;
    ext[5..15].copy_from_slice(&[0x03, 0x0c, 0x00, 0x10, 0x00, 0x00, 0x00, latency_flags, 11, 0]);
    edid
}

#[test]
fn hdmi_vendor_block_latency_is_read() {
    let lat = edid::hdmi_latency(&synthetic_edid(0x80));
    assert_eq!(lat, edid::HdmiLatency { vsdb_present: true, video_ms: Some(20), audio_ms: None });

    let no_fields = edid::hdmi_latency(&synthetic_edid(0x00));
    assert_eq!(no_fields, edid::HdmiLatency { vsdb_present: true, video_ms: None, audio_ms: None });

    let base_only = edid::hdmi_latency(&synthetic_edid(0x80)[..128]);
    assert_eq!(base_only, edid::HdmiLatency::default());

    assert_eq!(
        edid::describe(&synthetic_edid(0x80)),
        "mfg=SAM product=3021 bytes=256 hdmi_vsdb=1 video_latency_ms=20 audio_latency_ms=none"
    );
}

#[test]
fn ioreg_edid_blobs_are_found_once_each() {
    let hex: String = synthetic_edid(0x80).iter().map(|b| format!("{b:02x}")).collect();
    let text = format!(
        "    | |   \"EDID\" = <{hex}>\n    | |   \"IODisplayEDID\"=<{hex}>\n  \"EDID\" = <00ff>\n"
    );
    let blobs = edid::find_hex_blobs(&text);
    assert_eq!(blobs, vec![synthetic_edid(0x80)]);
}
