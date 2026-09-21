// Scratch integration test: does ProfileSession::stop_and_dump actually write?
use manifold_profiler::*;

#[test]
fn comparisons_do_not_invent_percentages_for_zero_baselines() {
    let mut paths = Vec::new();
    for (name, work) in [("compare-zero", 0.0), ("compare-nonzero", 5.0)] {
        let mut session = ProfileSession::new(name.into(), "p".into(), (1, 1), 24.0, "gpu".into());
        session.record_frame(frame(0, work, None));
        paths.push(session.stop_and_dump().unwrap());
    }
    let comparison = compare::compare_sessions(&paths[0], &paths[1]).unwrap();
    assert_eq!(comparison.overall_delta_ms, 5.0);
    assert_eq!(comparison.overall_delta_pct, None);
    assert_eq!(comparison.budget_improvement_pct, None);
    let summary_path = paths[0].join("summary.json");
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    legacy["schema_version"] = serde_json::json!(3);
    std::fs::write(summary_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(compare::compare_sessions(&paths[0], &paths[1]).is_err());
}

#[test]
fn dump_writes_all_four_files() {
    let dir = std::env::temp_dir().join(format!("prof-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();

    let mut s = ProfileSession::new(
        "SmokeTest".into(),
        "/tmp/x.manifold".into(),
        (1920, 1080),
        60.0,
        "Metal GPU".into(),
    );
    for i in 0..3u64 {
        s.record_frame(FrameRecord {
            index: i,
            beat: i as f32 * 0.5,
            bar: 0,
            wall_time_ms: 5.0 + i as f64,
            budget_exceeded: false,
            pacing: (i > 0).then_some(FramePacing {
                interval_ms: 70.0,
                target_interval_ms: 1000.0 / 24.0,
                deadline_lateness_ms: 70.0 - 1000.0 / 24.0,
            }),
            content_thread: ContentTimings {
                total_ms: 5.0,
                midi_input_ms: 0.1,
                sync_controllers_ms: 0.2,
                engine_tick_ms: 1.0,
                render_content_ms: 3.0,
                gpu_poll_ms: 0.5,
                cleanup_ms: 0.2,
                prelude_ms: i as f64,
                state_publish_ms: 2.0 * i as f64,
            },
            gpu_passes: vec![],
            active_clips: vec![],
            active_effects: vec![],
            active_layer_count: 1,
            gpu_pass_count: None,
            gpu_total_ms: None,
            layer_states: vec![],
            missed_frames: 0,
            profiler_overhead_ms: 0.0,
            memory: MemorySnapshot {
                estimated_texture_bytes: 0,
                render_target_count: 0,
            },
        });
    }
    let path = s.stop_and_dump().expect("dump must succeed");
    for f in ["session.json", "summary.json", "frames.jsonl"] {
        assert!(path.join(f).exists(), "{f} missing in {}", path.display());
    }
    let summary: SessionSummary =
        serde_json::from_str(&std::fs::read_to_string(path.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary.phase_aggregates.prelude.mean_ms, 1.0);
    assert_eq!(summary.phase_aggregates.state_publish.max_ms, 4.0);
    assert_eq!(summary.pacing.as_ref().unwrap().sample_count, 2);
    assert!(summary.jitter.is_some());
    assert!(summary.pass_count.is_none());
    println!("dump OK at {}", path.display());
}

fn frame(index: u64, work_ms: f64, pacing: Option<FramePacing>) -> FrameRecord {
    FrameRecord {
        index,
        beat: index as f32,
        bar: 0,
        wall_time_ms: work_ms,
        budget_exceeded: work_ms > 41.666,
        pacing,
        content_thread: ContentTimings {
            total_ms: work_ms,
            ..ContentTimings::default()
        },
        gpu_passes: vec![],
        active_clips: vec![],
        active_effects: vec![],
        active_layer_count: 0,
        gpu_pass_count: None,
        gpu_total_ms: None,
        layer_states: vec![],
        missed_frames: 0,
        profiler_overhead_ms: 0.0,
        memory: MemorySnapshot::default(),
    }
}

#[test]
fn pacing_is_independent_of_content_work_and_lateness_is_retained() {
    let mut s = ProfileSession::new("pacing".into(), "p".into(), (1, 1), 24.0, "gpu".into());
    s.record_frame(frame(
        0,
        5.0,
        Some(FramePacing {
            interval_ms: 70.0,
            target_interval_ms: 1000.0 / 24.0,
            deadline_lateness_ms: 70.0 - 1000.0 / 24.0,
        }),
    ));
    s.record_frame(frame(
        1,
        5.0,
        Some(FramePacing {
            interval_ms: 70.0,
            target_interval_ms: 1000.0 / 24.0,
            deadline_lateness_ms: 70.0 - 1000.0 / 24.0,
        }),
    ));
    let dir = s.stop_and_dump().unwrap();
    let summary: SessionSummary =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    let pacing = summary.pacing.unwrap();
    assert_eq!(pacing.interval_ms.mean_ms, 70.0);
    assert_eq!(pacing.late_intervals, 2);
    assert_eq!(summary.mean_frame_ms, 5.0);
    assert_eq!(summary.jitter.unwrap().mean_dt_ms, 70.0);
}

#[test]
fn missing_gpu_and_legacy_pacing_are_unavailable_not_zero() {
    let mut s = ProfileSession::new("missing".into(), "p".into(), (1, 1), 24.0, "gpu".into());
    s.record_frame(frame(0, 5.0, None));
    let dir = s.stop_and_dump().unwrap();
    let summary: SessionSummary =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert!(summary.pacing.is_none());
    assert!(summary.jitter.is_none());
    assert!(summary.pass_count.is_none());
    let json = std::fs::read_to_string(dir.join("frames.jsonl")).unwrap();
    assert!(json.contains("\"gpu_pass_count\":null"));
    assert!(json.contains("\"gpu_total_ms\":null"));
}

#[test]
fn serialization_uses_truthful_labels_and_schema_version() {
    let mut s = ProfileSession::new("labels".into(), "p".into(), (1, 1), 24.0, "gpu".into());
    s.record_frame(frame(0, 5.0, None));
    let dir = s.stop_and_dump().unwrap();
    let frame_json = std::fs::read_to_string(dir.join("frames.jsonl")).unwrap();
    let summary_json = std::fs::read_to_string(dir.join("summary.json")).unwrap();
    let session_json = std::fs::read_to_string(dir.join("session.json")).unwrap();
    assert!(frame_json.contains("\"content_work_ms\""));
    assert!(frame_json.contains("\"content_work_budget_exceeded\""));
    assert!(frame_json.contains("\"gpu_surface_wait_ms\""));
    assert!(summary_json.contains("\"content_work_mean_ms\""));
    assert!(summary_json.contains("\"worst_content_work\""));
    assert!(session_json.contains("\"schema_version\": 4"));
    assert!(session_json.contains("\"presentation\": \"not_measured\""));
}

#[test]
fn invalid_pacing_is_reported_and_not_turned_into_zero() {
    let mut s = ProfileSession::new("invalid".into(), "p".into(), (1, 1), 24.0, "gpu".into());
    s.record_frame(frame(
        0,
        5.0,
        Some(FramePacing {
            interval_ms: 0.0,
            target_interval_ms: 41.0,
            deadline_lateness_ms: 0.0,
        }),
    ));
    let dir = s.stop_and_dump().unwrap();
    let summary: SessionSummary =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert!(summary.pacing.is_none());
    assert!(
        summary
            .recommendations
            .iter()
            .any(|r| r.contains("unavailable"))
    );
}

#[test]
fn legacy_content_timings_keep_existing_values_and_default_new_phases() {
    let timings: ContentTimings = serde_json::from_str(
        r#"{
        "total_ms":5.0,"midi_input_ms":0.1,"sync_controllers_ms":0.2,
        "engine_tick_ms":1.0,"render_content_ms":3.0,"gpu_poll_ms":150.0,
        "cleanup_ms":0.2
    }"#,
    )
    .unwrap();
    assert_eq!(timings.total_ms, 5.0);
    assert_eq!(timings.gpu_poll_ms, 150.0);
    assert_eq!(timings.prelude_ms, 0.0);
    assert_eq!(timings.state_publish_ms, 0.0);
}

#[test]
fn measured_gpu_zero_stays_distinct_from_unavailable() {
    let mut s = ProfileSession::new(
        "measured-zero".into(),
        "p".into(),
        (1, 1),
        24.0,
        "gpu".into(),
    );
    let mut measured = frame(0, 1.0, None);
    measured.gpu_pass_count = Some(0);
    measured.gpu_total_ms = Some(0.0);
    s.record_frame(measured);
    let dir = s.stop_and_dump().unwrap();
    let summary: SessionSummary =
        serde_json::from_slice(&std::fs::read(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary.pass_count.unwrap().mean_gpu_total_ms, 0.0);
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("frames.jsonl")).unwrap()).unwrap();
    assert_eq!(record["gpu_total_ms"], 0.0);
}

#[test]
fn partial_or_inconsistent_pacing_cannot_produce_a_complete_summary() {
    for (name, bad) in [
        ("missing-interval", None),
        (
            "inconsistent-interval",
            Some(FramePacing {
                interval_ms: 70.0,
                target_interval_ms: 40.0,
                deadline_lateness_ms: 0.0,
            }),
        ),
    ] {
        let mut s = ProfileSession::new(name.into(), "p".into(), (1, 1), 25.0, "gpu".into());
        s.record_frame(frame(
            0,
            5.0,
            Some(FramePacing {
                interval_ms: 40.0,
                target_interval_ms: 40.0,
                deadline_lateness_ms: 0.0,
            }),
        ));
        s.record_frame(frame(1, 5.0, bad));
        let dir = s.stop_and_dump().unwrap();
        let summary: SessionSummary =
            serde_json::from_slice(&std::fs::read(dir.join("summary.json")).unwrap()).unwrap();
        assert!(summary.pacing.is_none());
        assert!(summary.jitter.is_none());
    }
}

#[test]
fn invalid_content_work_cannot_turn_into_a_zero_summary() {
    let mut s = ProfileSession::new(
        "invalid-work".into(),
        "p".into(),
        (1, 1),
        24.0,
        "gpu".into(),
    );
    s.record_frame(frame(0, f64::NAN, None));
    assert!(
        s.stop_and_dump()
            .unwrap_err()
            .contains("Invalid timing measurement")
    );
}

#[test]
fn first_use_spike_reports_observation_without_claiming_compilation() {
    let mut s = ProfileSession::new("first-use".into(), "p".into(), (1, 1), 24.0, "gpu".into());
    // An unsorted sequence ensures first occurrence is not confused with
    // the maximum or the sorted timing order.
    for (index, ms) in [100.0, 1.0, 2.0].into_iter().enumerate() {
        let mut record = frame(index as u64, 5.0, None);
        record.gpu_pass_count = Some(1);
        record.gpu_total_ms = Some(ms);
        record.gpu_passes.push(GpuPassRecord {
            name: "test pass".into(),
            ms,
            begin_ns: 0.0,
            end_ns: ms * 1e6,
            width: 1,
            height: 1,
            is_compute: true,
        });
        s.record_frame(record);
    }
    let dir = s.stop_and_dump().unwrap();
    let summary: SessionSummary =
        serde_json::from_slice(&std::fs::read(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary.first_use_spikes[0].first_use_frame, 0);
    assert_eq!(summary.first_use_spikes[0].steady_state_mean_ms, 1.5);
    assert!(
        summary
            .recommendations
            .iter()
            .any(|text| text.contains("cause unknown"))
    );
    assert!(
        summary
            .recommendations
            .iter()
            .all(|text| !text.contains("compilation"))
    );
}
