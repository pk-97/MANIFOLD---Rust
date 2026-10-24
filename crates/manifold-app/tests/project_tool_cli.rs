//! `project_tool` timeline verbs end to end: the real binary on a real
//! archive, read back through the typed loader.

use std::path::{Path, PathBuf};
use std::process::Command;

use manifold_core::types::{LayerType, MarkerColor};
use manifold_io::loader::load_project;
use serde_json::{Value, json};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("project_tool_cli_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    fn json(&self, name: &str, v: &Value) -> String {
        let p = self.file(name);
        std::fs::write(&p, v.to_string()).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// (exit code, stdout).
fn tool(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_project_tool"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
    )
}

fn ok(args: &[&str]) -> String {
    let (code, stdout) = tool(args);
    assert_eq!(code, 0, "project_tool {args:?} failed");
    stdout
}

fn ok_json(args: &[&str]) -> Value {
    serde_json::from_str(&ok(args)).unwrap()
}

/// A project with Mix, Drums, and two trigger lanes under Drums.
fn review_project(s: &Scratch) -> String {
    let f = s.file("review.manifold");
    ok(&["project", "new", &f, "--bpm", "128"]);
    ok(&[
        "layer", "add", &f, "--name", "Mix", "--type", "audio", "--color", "#336699",
    ]);
    ok(&[
        "layer",
        "add",
        &f,
        "--name",
        "Drums",
        "--type",
        "audio",
        "--collapsed",
    ]);
    ok(&[
        "layer",
        "add",
        &f,
        "--name",
        "Kick labels",
        "--type",
        "trigger",
        "--parent",
        "Drums",
    ]);
    ok(&[
        "layer",
        "add",
        &f,
        "--name",
        "Snare labels",
        "--type",
        "trigger",
        "--parent",
        "Drums",
    ]);
    f
}

#[test]
fn project_new_writes_a_loadable_empty_project_and_never_overwrites() {
    let s = Scratch::new("new");
    let f = s.file("empty.manifold");
    ok(&["project", "new", &f, "--bpm", "140"]);
    let p = load_project(Path::new(&f)).unwrap();
    assert!(p.timeline.layers.is_empty());
    assert_eq!(p.settings.bpm.0, 140.0);
    assert_eq!(tool(&["project", "new", &f]).0, 1);
    assert_eq!(
        tool(&["project", "new", &s.file("x.manifold"), "--bpm", "500"]).0,
        1
    );
}

#[test]
fn layer_add_models_trigger_lanes_like_the_app() {
    let s = Scratch::new("layers");
    let f = review_project(&s);
    let group = ok(&["layer", "add", &f, "--name", "Ideas", "--type", "group"]);
    let generator = ok(&[
        "layer",
        "add",
        &f,
        "--name",
        "Visual",
        "--type",
        "generator",
        "--parent",
        "Ideas",
    ]);
    let p = load_project(Path::new(&f)).unwrap();
    let names: Vec<&str> = p.timeline.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Mix",
            "Drums",
            "Kick labels",
            "Snare labels",
            "Ideas",
            "Visual"
        ]
    );
    let drums = &p.timeline.layers[1];
    assert!(drums.is_collapsed);
    for lane in &p.timeline.layers[2..4] {
        assert_eq!(lane.layer_type, LayerType::Trigger);
        assert_eq!(lane.parent_layer_id.as_ref(), Some(&drums.layer_id));
    }
    let mix = &p.timeline.layers[0];
    assert!(
        (mix.layer_color.r - 0x33 as f32 / 255.0).abs() < 1e-6
            && (mix.layer_color.b - 0x99 as f32 / 255.0).abs() < 1e-6
    );
    assert_eq!(p.timeline.layers[4].layer_id.as_str(), group.trim());
    assert_eq!(p.timeline.layers[5].layer_id.as_str(), generator.trim());
    assert_eq!(p.timeline.layers[5].layer_type, LayerType::Generator);
    // Refusals leave the file alone.
    assert_eq!(
        tool(&["layer", "add", &f, "--name", "Loose", "--type", "trigger"]).0,
        1
    );
    assert_eq!(
        tool(&["layer", "add", &f, "--name", "Mix", "--type", "audio"]).0,
        1
    );
    assert_eq!(
        tool(&[
            "layer", "add", &f, "--name", "X", "--type", "audio", "--bogus", "1"
        ])
        .0,
        2
    );
    assert_eq!(
        load_project(Path::new(&f)).unwrap().timeline.layers.len(),
        6
    );
}

#[test]
fn clip_add_bulk_round_trips_and_returns_ids_in_input_order() {
    let s = Scratch::new("clips");
    let f = review_project(&s);
    let lanes = s.json(
        "lanes.json",
        &json!([
            {"start_beat": 8.0, "duration_beats": 0.25, "name": "late"},
            {"start_beat": 0.0, "duration_beats": 0.25},
            {"start_beat": 0.25, "duration_beats": 0.25},
        ]),
    );
    let ids = ok_json(&[
        "clip",
        "add-bulk",
        &f,
        "--layer",
        "Kick labels",
        "--json",
        &lanes,
    ]);
    let audio = s.json("audio.json", &json!([
        {"start_beat": 0.0, "duration_beats": 64.0, "path": "/music/a.wav", "in_point": 1.5, "source_duration": 30.0},
    ]));
    let audio_ids = ok_json(&["clip", "add-bulk", &f, "--layer", "Mix", "--json", &audio]);

    let p = load_project(Path::new(&f)).unwrap();
    let lane = p
        .timeline
        .layers
        .iter()
        .find(|l| l.name == "Kick labels")
        .unwrap();
    let start_of = |id: &Value| {
        lane.clips
            .iter()
            .find(|c| c.id.as_str() == id.as_str().unwrap())
            .unwrap()
            .start_beat
            .0
    };
    let starts: Vec<f64> = ids.as_array().unwrap().iter().map(start_of).collect();
    assert_eq!(starts, [8.0, 0.0, 0.25]);
    let mix = p.timeline.layers.iter().find(|l| l.name == "Mix").unwrap();
    let a = &mix.clips[0];
    assert_eq!(a.id.as_str(), audio_ids[0].as_str().unwrap());
    assert_eq!(
        (
            a.audio_file_path.as_str(),
            a.in_point.0,
            a.source_duration.0
        ),
        ("/music/a.wav", 1.5, 30.0)
    );
}

#[test]
fn clip_add_bulk_refuses_any_overlap_and_writes_nothing() {
    let s = Scratch::new("overlap");
    let f = review_project(&s);
    let first = s.json(
        "first.json",
        &json!([{"start_beat": 4.0, "duration_beats": 1.0}]),
    );
    ok(&[
        "clip",
        "add-bulk",
        &f,
        "--layer",
        "Kick labels",
        "--json",
        &first,
    ]);
    let before = std::fs::read(&f).unwrap();
    let inner = s.json(
        "inner.json",
        &json!([
            {"start_beat": 0.0, "duration_beats": 1.0},
            {"start_beat": 0.5, "duration_beats": 1.0},
        ]),
    );
    assert_eq!(
        tool(&[
            "clip",
            "add-bulk",
            &f,
            "--layer",
            "Kick labels",
            "--json",
            &inner
        ])
        .0,
        1
    );
    let existing = s.json(
        "existing.json",
        &json!([{"start_beat": 4.5, "duration_beats": 0.1}]),
    );
    assert_eq!(
        tool(&[
            "clip",
            "add-bulk",
            &f,
            "--layer",
            "Kick labels",
            "--json",
            &existing
        ])
        .0,
        1
    );
    assert_eq!(
        std::fs::read(&f).unwrap(),
        before,
        "a refused add must not touch the file"
    );
    let touching = s.json(
        "touching.json",
        &json!([
            {"start_beat": 3.0, "duration_beats": 1.0},
            {"start_beat": 5.0, "duration_beats": 1.0},
        ]),
    );
    ok(&[
        "clip",
        "add-bulk",
        &f,
        "--layer",
        "Kick labels",
        "--json",
        &touching,
    ]);
}

#[test]
fn markers_round_trip_with_ids_in_input_order() {
    let s = Scratch::new("markers");
    let f = review_project(&s);
    let m = s.json(
        "m.json",
        &json!([
            {"beat": 64.0, "name": "Song B", "color": "#00ff00"},
            {"beat": 0.0, "name": "Song A", "color": "red"},
            {"beat": 32.0, "name": "Song A drop"},
        ]),
    );
    let ids = ok_json(&["marker", "add-bulk", &f, "--json", &m]);
    let p = load_project(Path::new(&f)).unwrap();
    let marker = |i: usize| {
        p.timeline
            .markers
            .iter()
            .find(|m| m.id.as_str() == ids[i].as_str().unwrap())
            .unwrap()
    };
    assert_eq!(
        (marker(0).name.as_str(), marker(0).color),
        ("Song B", MarkerColor::Green)
    );
    assert_eq!(
        (marker(1).name.as_str(), marker(1).color),
        ("Song A", MarkerColor::Red)
    );
    assert_eq!(marker(2).beat.0, 32.0);
    let beats: Vec<f64> = p.timeline.markers.iter().map(|m| m.beat.0).collect();
    assert_eq!(beats, [0.0, 32.0, 64.0]);
}

/// The seconds `tempo at` prints for `beat`.
fn tempo_at(f: &str, beat: f64) -> f64 {
    let out = ok(&["tempo", "at", f, &beat.to_string()]);
    let s = out
        .trim()
        .rsplit(" = ")
        .next()
        .unwrap()
        .trim_end_matches('s');
    s.parse().unwrap()
}

#[test]
fn lists_report_seconds_through_the_tempo_map_like_tempo_at() {
    let s = Scratch::new("lists");
    let f = review_project(&s);
    // Hundreds of points, one tempo window per "song".
    let points: Vec<Value> = (0..400)
        .map(|i| json!({"beat": i as f64 * 16.0, "bpm": 90.0 + (i % 7) as f64 * 11.5}))
        .collect();
    let tempo = s.json("tempo.json", &Value::Array(points));
    ok(&["tempo", "set", &f, &tempo]);
    let clips = s.json(
        "c.json",
        &json!([
            {"start_beat": 15.5, "duration_beats": 1.0},
            {"start_beat": 3000.25, "duration_beats": 0.5},
        ]),
    );
    ok(&[
        "clip",
        "add-bulk",
        &f,
        "--layer",
        "Snare labels",
        "--json",
        &clips,
    ]);
    let m = s.json("m.json", &json!([{"beat": 777.0, "name": "here"}]));
    ok(&["marker", "add-bulk", &f, "--json", &m]);

    let rows = ok_json(&["clip", "list", &f, "--json"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        assert_eq!(r["layer"], "Snare labels");
        let (start, dur) = (
            r["start_beat"].as_f64().unwrap(),
            r["duration_beats"].as_f64().unwrap(),
        );
        assert!((r["start_s"].as_f64().unwrap() - tempo_at(&f, start)).abs() < 1e-5);
        assert!((r["end_s"].as_f64().unwrap() - tempo_at(&f, start + dur)).abs() < 1e-5);
        assert!(r["name"].is_null());
    }
    let markers = ok_json(&["marker", "list", &f, "--json"]);
    assert_eq!(markers[0]["name"], "here");
    assert!((markers[0]["seconds"].as_f64().unwrap() - tempo_at(&f, 777.0)).abs() < 1e-5);
    assert_eq!(
        load_project(Path::new(&f)).unwrap().tempo_map.point_count(),
        400
    );
}

fn tool_stderr(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_project_tool"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "project_tool {args:?} failed");
    String::from_utf8(out.stderr).unwrap()
}

/// The bin must link the bundled preset registry (`use manifold_nodes as _;`):
/// with it, a generator layer's unknown param id is dropped with a warning and
/// a bundled id is kept; without it nothing resolves and nothing is warned.
#[test]
fn info_resolves_bundled_generator_params_through_the_linked_registry() {
    let s = Scratch::new("registry");
    let f = s.file("scene.manifold");
    ok(&["project", "new", &f, "--bpm", "120"]);
    ok(&[
        "layer",
        "add",
        &f,
        "--name",
        "Shapes",
        "--type",
        "generator",
    ]);
    let mut root = ok_json(&["json", &f]);
    root["timeline"]["layers"][0]["genParams"] = json!({
        "generatorType": "BasicShapes",
        "params": {
            "line": {"value": 1.0, "exposed": true},
            "no_such_param_zz": {"value": 1.0, "exposed": true},
        },
    });
    // A plain-JSON (V1) file is a valid input to the typed loader.
    let plain = s.json("plain.manifold", &root);
    let stderr = tool_stderr(&["info", &plain]);
    assert!(
        stderr.contains("dropping unknown param id \"no_such_param_zz\""),
        "registry not linked: bogus id was not dropped. stderr: {stderr}"
    );
    assert!(
        !stderr.contains("dropping unknown param id \"line\""),
        "bundled id wrongly dropped: {stderr}"
    );
}
