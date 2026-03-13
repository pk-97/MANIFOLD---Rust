use manifold_io::loader;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

#[test]
fn load_burn_v5_project() {
    let path = fixture_path("Burn V5.manifold");
    assert!(path.exists(), "Test fixture not found: {}", path.display());

    let project = loader::load_project(&path)
        .expect("Failed to load Burn V5.manifold");

    // Basic project-level assertions
    assert_eq!(project.project_name, "Burn V5");

    // Settings
    assert!((project.settings.bpm - 138.0).abs() < 0.01, "BPM should be 138.0, got {}", project.settings.bpm);
    assert_eq!(project.settings.output_width, 1440);
    assert_eq!(project.settings.output_height, 2560);

    // Timeline structure
    assert_eq!(project.timeline.layers.len(), 9, "Expected 9 layers");

    // Layer names
    let layer_names: Vec<&str> = project.timeline.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(layer_names, vec!["Gen 2", "Gen 3", "LISSAJOUS", "Gen 3", "Gen 4", "TUNNELS 2", "TESSERACT", "FIRE", "STOCK"]);

    // Total clip count
    assert_eq!(project.timeline.total_clip_count(), 34, "Expected 34 total clips");
}

#[test]
fn load_burn_v5_clips_have_valid_beats() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    for (li, layer) in project.timeline.layers.iter().enumerate() {
        for (ci, clip) in layer.clips.iter().enumerate() {
            // IDs must not be empty
            assert!(!clip.id.is_empty(), "Clip [{li}][{ci}] has empty ID");

            // Duration must be positive
            assert!(clip.duration_beats > 0.0,
                "Clip {} [{li}][{ci}] has non-positive duration: {}",
                clip.id, clip.duration_beats);

            // Start beat should be non-negative
            assert!(clip.start_beat >= 0.0,
                "Clip {} [{li}][{ci}] has negative start beat: {}",
                clip.id, clip.start_beat);

            // end_beat should be after start_beat
            assert!(clip.end_beat() > clip.start_beat,
                "Clip {} [{li}][{ci}] end beat {} <= start beat {}",
                clip.id, clip.end_beat(), clip.start_beat);
        }
    }
}

#[test]
fn load_burn_v5_timeline_duration() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    let duration = project.timeline.duration_beats();
    assert!(duration > 0.0, "Timeline should have positive duration, got {}", duration);
}

#[test]
fn load_burn_v5_clip_lookup_works() {
    let path = fixture_path("Burn V5.manifold");
    let mut project = loader::load_project(&path).unwrap();

    // Grab a clip ID from the first layer
    let first_clip_id = project.timeline.layers[0].clips[0].id.clone();

    // O(1) lookup should find it
    let found = project.timeline.find_clip_by_id(&first_clip_id);
    assert!(found.is_some(), "Clip lookup failed for ID: {first_clip_id}");
    assert_eq!(found.unwrap().id, first_clip_id);
}

#[test]
fn load_burn_v5_roundtrip_json() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    // Serialize back to JSON
    let json = serde_json::to_string_pretty(&project)
        .expect("Failed to serialize project");

    // Reload from the serialized JSON
    let project2 = loader::load_project_from_json(&json)
        .expect("Failed to reload from serialized JSON");

    // Basic structural equivalence
    assert_eq!(project2.project_name, project.project_name);
    assert_eq!(project2.settings.bpm, project.settings.bpm);
    assert_eq!(project2.timeline.layers.len(), project.timeline.layers.len());
    assert_eq!(project2.timeline.total_clip_count(), project.timeline.total_clip_count());
}

// ── Additional project files ──

#[test]
fn load_burn_v4_project() {
    let path = fixture_path("Burn V4.manifold");
    if !path.exists() { return; } // Skip if fixture not available

    let project = loader::load_project(&path)
        .expect("Failed to load Burn V4.manifold");

    assert_eq!(project.project_name, "Burn V4");
    assert!((project.settings.bpm - 138.0).abs() < 0.01);
    assert_eq!(project.timeline.layers.len(), 9);
    assert_eq!(project.timeline.total_clip_count(), 34);
}

#[test]
fn load_waypoints_large_project() {
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() { return; }

    let project = loader::load_project(&path)
        .expect("Failed to load WAYPOINTS.manifold");

    assert_eq!(project.project_name, "WAYPOINTS");
    assert!((project.settings.bpm - 110.0).abs() < 0.01);
    assert_eq!(project.timeline.layers.len(), 9);
    assert_eq!(project.timeline.total_clip_count(), 2311);

    // Stress test: all clips should have valid beats
    for layer in &project.timeline.layers {
        for clip in &layer.clips {
            assert!(clip.duration_beats > 0.0);
            assert!(clip.start_beat >= 0.0);
        }
    }
}
