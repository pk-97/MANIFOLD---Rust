// Scratch integration test: does ProfileSession::stop_and_dump actually write?
use manifold_profiler::*;

#[test]
fn dump_writes_all_four_files() {
    let dir = std::env::temp_dir().join(format!("prof-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();

    let mut s = ProfileSession::new(
        "SmokeTest".into(), "/tmp/x.manifold".into(), (1920, 1080), 60.0, "Metal GPU".into(),
    );
    for i in 0..3u64 {
        s.record_frame(FrameRecord {
            index: i, beat: i as f32 * 0.5, bar: 0, wall_time_ms: 5.0 + i as f64,
            budget_exceeded: false,
            content_thread: ContentTimings { total_ms: 5.0, midi_input_ms: 0.1,
                sync_controllers_ms: 0.2, engine_tick_ms: 1.0, render_content_ms: 3.0,
                gpu_poll_ms: 0.5, cleanup_ms: 0.2 },
            gpu_passes: vec![], active_clips: vec![], active_effects: vec![],
            active_layer_count: 1, gpu_pass_count: 0, gpu_total_ms: 0.0,
            layer_states: vec![], missed_frames: 0, profiler_overhead_ms: 0.0,
            memory: MemorySnapshot { estimated_texture_bytes: 0, render_target_count: 0 },
        });
    }
    let path = s.stop_and_dump().expect("dump must succeed");
    for f in ["session.json", "summary.json", "frames.jsonl"] {
        assert!(path.join(f).exists(), "{f} missing in {}", path.display());
    }
    println!("dump OK at {}", path.display());
}
