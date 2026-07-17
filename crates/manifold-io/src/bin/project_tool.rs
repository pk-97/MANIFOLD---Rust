//! `project_tool` — CLI verbs for agents inspecting and modifying `.manifold`
//! project files (sibling of `graph_tool`, which owns graph JSON).
//!
//! ```text
//! project_tool info <file.manifold>
//! project_tool json <file.manifold>
//! project_tool tempo show <file.manifold>
//! project_tool tempo set <file.manifold> <points.json> [--no-sync-bpm]
//! project_tool tempo at <file.manifold> <beat> [<beat>...]
//! project_tool clip add-audio <file.manifold> --layer <name> --path <audio>
//!     --start-beat <B> --duration-beats <B> [--in-point <s>] [--source-duration <s>]
//! ```
//!
//! ## Why mutations are raw-JSON surgery, not model round-trips
//!
//! A headless CLI has no preset/generator registry, so a typed
//! `load_project` → `save_project` round-trip **drops** every param the
//! registry can't resolve ("dropping unknown param id ..."), silently
//! stripping performance data from the file. Mutation verbs therefore edit
//! the raw `project.json` inside the archive as `serde_json::Value` —
//! touching only the keys they own (`tempoMap`, `settings.bpm`, one layer's
//! `clips`) and leaving every other byte of structure alone. The typed
//! loader still runs on the edited JSON as a **validation gate** (its
//! droppy reconcile is in-memory only, and its param warnings on stderr are
//! expected and harmless); the file is then written with
//! [`archive::save_v2_archive`] — the app's own writer — so hashing, dedup,
//! history snapshots, and the atomic temp-file rename all behave exactly
//! like an in-app save. A V1 plain-JSON file is upgraded to a V2 archive on
//! its first mutation, matching app behavior.
//!
//! `tempo set` replaces the whole tempo map from a JSON array of points
//! (`[{"beat": 0.0, "bpm": 132.0, "source": 4}, ...]` — `TempoPoint` serde,
//! camelCase, `source`/`recordedAtSeconds` optional). MANIFOLD's map is
//! piecewise-constant (step BPM between points); BPM outside the playable
//! 20–300 range is rejected loudly rather than silently clamped. By default
//! `settings.bpm` is re-synced to the map's beat-0 value, matching the
//! serializer contract (`sync_bpm_from_tempo_map`).
//!
//! `tempo at` converts beats to seconds with the production converter
//! (`TempoMapConverter::beat_to_seconds_immut`) — the verification oracle
//! for any externally-derived map (e.g. an Ableton import).
//!
//! `clip add-audio` refuses to overlap an existing clip on the target layer
//! (write-time non-overlap is a `Layer` invariant; interactive trimming is
//! the app's job, not this tool's).
//!
//! Exit codes: `0` ok, `1` validation/lookup failure, `2` usage / IO / parse.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use manifold_core::clip::TimelineClip;
use manifold_core::tempo::{TempoMap, TempoMapConverter, TempoPoint};
use manifold_core::units::{Beats, Bpm, Seconds};
use manifold_io::archive::save_v2_archive;
use manifold_io::loader::{load_project, load_project_from_json};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((verb, rest)) = args.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };

    match verb.as_str() {
        "info" => run_info(rest),
        "json" => run_json(rest),
        "tempo" => run_tempo(rest),
        "clip" => run_clip(rest),
        "-h" | "--help" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown verb '{other}'\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!(
        "project_tool — inspect and modify .manifold project files

Mutations are surgical raw-JSON edits (no typed round-trip, nothing else in
the file is rewritten), validated through the real loader, written with the
app's own archive writer (history + atomic rename preserved).

USAGE:
  project_tool info <file.manifold>
  project_tool json <file.manifold>
  project_tool tempo show <file.manifold>
  project_tool tempo set <file.manifold> <points.json> [--no-sync-bpm]
  project_tool tempo at <file.manifold> <beat> [<beat>...]
  project_tool clip add-audio <file.manifold> --layer <name> --path <audio>
      --start-beat <B> --duration-beats <B> [--in-point <s>] [--source-duration <s>]"
    );
}

// ── raw project.json access ─────────────────────────────────────────────

/// Read the current `project.json` text from a V2 archive, or the whole file
/// for a V1 plain-JSON project.
fn read_raw_json(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read '{path}': {e}"))?;
    let cursor = std::io::Cursor::new(&bytes);
    match zip::ZipArchive::new(cursor) {
        Ok(mut archive) => {
            let mut entry = archive
                .by_name("project.json")
                .map_err(|e| format!("archive has no project.json: {e}"))?;
            let mut json = String::new();
            entry
                .read_to_string(&mut json)
                .map_err(|e| format!("failed to read project.json: {e}"))?;
            Ok(json)
        }
        Err(_) => String::from_utf8(bytes).map_err(|e| format!("not a ZIP and not UTF-8: {e}")),
    }
}

fn parse_root(json: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(json).map_err(|e| format!("project.json parse failed: {e}"))
}

/// Validate edited JSON through the real typed loader (in-memory only — its
/// registry-less param drops don't touch the file), then write it back with
/// the app's own archive writer.
fn validate_and_save(root: &serde_json::Value, path: &str) -> ExitCode {
    let json = match serde_json::to_string_pretty(root) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: serialize failed: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("(validating through typed loader — param-spec warnings below are expected)");
    if let Err(e) = load_project_from_json(&json) {
        eprintln!("error: edited project fails to load, NOT saving: {e}");
        return ExitCode::FAILURE;
    }
    let name = root
        .get("projectName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match save_v2_archive(&json, name, path, Some("project_tool"), false) {
        Ok(_) => {
            println!("saved: {path}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: failed to save '{path}': {e}");
            ExitCode::from(2)
        }
    }
}

// ── info (typed load, read-only) ────────────────────────────────────────

fn run_info(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    let project = match load_project(Path::new(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to load '{path}': {e}");
            return ExitCode::from(2);
        }
    };

    println!("project:  {}", project.project_name);
    println!("version:  {}", project.project_version);
    println!(
        "settings: bpm {} · {}/{} · {}x{} @ {}fps",
        project.settings.bpm.0,
        project.settings.time_signature_numerator,
        project.settings.time_signature_denominator,
        project.settings.output_width,
        project.settings.output_height,
        project.settings.frame_rate,
    );
    println!("tempo:    {} point(s)", project.tempo_map.point_count());
    let layers = &project.timeline.layers;
    let clip_total: usize = layers.iter().map(|l| l.clips.len()).sum();
    println!("timeline: {} layer(s), {} clip(s)", layers.len(), clip_total);
    for layer in layers {
        if layer.clips.is_empty() {
            continue;
        }
        let first = layer
            .clips
            .iter()
            .map(|c| c.start_beat)
            .fold(Beats(f64::MAX), Beats::min);
        let last = layer
            .clips
            .iter()
            .map(|c| c.end_beat())
            .fold(Beats::ZERO, Beats::max);
        println!(
            "  [{:>3}] {:?} '{}' — {} clip(s), beats {:.2}..{:.2}",
            layer.index,
            layer.layer_type,
            layer.name,
            layer.clips.len(),
            first.0,
            last.0,
        );
    }
    ExitCode::SUCCESS
}

// ── json ────────────────────────────────────────────────────────────────

fn run_json(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match read_raw_json(path) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

// ── tempo ───────────────────────────────────────────────────────────────

fn run_tempo(rest: &[String]) -> ExitCode {
    let Some((sub, rest)) = rest.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match sub.as_str() {
        "show" => tempo_show(rest),
        "set" => tempo_set(rest),
        "at" => tempo_at(rest),
        other => {
            eprintln!("error: unknown tempo subcommand '{other}'\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn read_tempo_context(path: &str) -> Result<(TempoMap, Bpm), String> {
    let root = parse_root(&read_raw_json(path)?)?;
    let map: TempoMap = root
        .get("tempoMap")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| format!("tempoMap parse failed: {e}"))?
        .unwrap_or_default();
    let fallback = root
        .pointer("/settings/bpm")
        .and_then(|v| v.as_f64())
        .unwrap_or(120.0) as f32;
    Ok((map, Bpm::clamped(fallback)))
}

fn tempo_show(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match read_tempo_context(path) {
        Ok((map, _)) => match serde_json::to_string_pretty(map.points()) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: serialize failed: {e}");
                ExitCode::from(2)
            }
        },
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn tempo_set(rest: &[String]) -> ExitCode {
    let (positional, flags): (Vec<&String>, Vec<&String>) =
        rest.iter().partition(|a| !a.starts_with("--"));
    let [path, points_path] = positional.as_slice() else {
        print_usage();
        return ExitCode::from(2);
    };
    let sync_bpm = !flags.iter().any(|f| f.as_str() == "--no-sync-bpm");

    let points_json = match std::fs::read_to_string(points_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read '{points_path}': {e}");
            return ExitCode::from(2);
        }
    };
    let mut points: Vec<TempoPoint> = match serde_json::from_str(&points_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to parse tempo points: {e}");
            return ExitCode::from(2);
        }
    };
    if points.is_empty() {
        eprintln!("error: refusing to set an empty tempo map");
        return ExitCode::FAILURE;
    }
    // ensure_valid would silently clamp to MANIFOLD's playable 20-300 range;
    // an import that needs clamping is a wrong import — fail loudly instead.
    for p in &points {
        if !p.bpm.0.is_finite() || !p.beat.is_finite() {
            eprintln!(
                "error: non-finite tempo point: beat {} bpm {}",
                p.beat.0, p.bpm.0
            );
            return ExitCode::FAILURE;
        }
        if !(20.0..=300.0).contains(&p.bpm.0) {
            eprintln!(
                "error: bpm {} at beat {} outside MANIFOLD's 20-300 range",
                p.bpm.0, p.beat.0
            );
            return ExitCode::FAILURE;
        }
    }
    points.sort_by(|a, b| {
        a.beat
            .partial_cmp(&b.beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let beat_zero_bpm = points[0].bpm;

    let mut root = match read_raw_json(path).and_then(|j| parse_root(&j)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let count = points.len();
    let points_value = match serde_json::to_value(&points) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: serialize points failed: {e}");
            return ExitCode::from(2);
        }
    };
    root["tempoMap"] = serde_json::json!({ "points": points_value });
    if sync_bpm && let Some(bpm_slot) = root.pointer_mut("/settings/bpm") {
        // sync_bpm_from_tempo_map contract: settings.bpm mirrors beat 0.
        *bpm_slot = serde_json::json!(beat_zero_bpm.0);
    }
    println!("tempo map: {count} point(s), beat-0 bpm {}", beat_zero_bpm.0);
    validate_and_save(&root, path)
}

fn tempo_at(rest: &[String]) -> ExitCode {
    let Some((path, beats)) = rest.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };
    if beats.is_empty() {
        print_usage();
        return ExitCode::from(2);
    }
    let (mut map, fallback) = match read_tempo_context(path) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    map.ensure_valid();
    for raw in beats {
        let Ok(beat) = raw.parse::<f64>() else {
            eprintln!("error: '{raw}' is not a number");
            return ExitCode::from(2);
        };
        let seconds = TempoMapConverter::beat_to_seconds_immut(&map, Beats(beat), fallback);
        println!("beat {beat} = {:.6}s", seconds.0);
    }
    ExitCode::SUCCESS
}

// ── clip ────────────────────────────────────────────────────────────────

fn run_clip(rest: &[String]) -> ExitCode {
    let Some((sub, rest)) = rest.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match sub.as_str() {
        "add-audio" => clip_add_audio(rest),
        other => {
            eprintln!("error: unknown clip subcommand '{other}'\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn flag_value<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1))
        .map(String::as_str)
}

fn clip_add_audio(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first().filter(|a| !a.starts_with("--")) else {
        print_usage();
        return ExitCode::from(2);
    };
    let (Some(layer_name), Some(audio_path), Some(start_beat), Some(duration_beats)) = (
        flag_value(rest, "--layer"),
        flag_value(rest, "--path"),
        flag_value(rest, "--start-beat"),
        flag_value(rest, "--duration-beats"),
    ) else {
        print_usage();
        return ExitCode::from(2);
    };
    let (Ok(start_beat), Ok(duration_beats)) =
        (start_beat.parse::<f64>(), duration_beats.parse::<f64>())
    else {
        eprintln!("error: --start-beat / --duration-beats must be numbers");
        return ExitCode::from(2);
    };
    let in_point: f64 = flag_value(rest, "--in-point")
        .unwrap_or("0")
        .parse()
        .unwrap_or(-1.0);
    let source_duration: f64 = flag_value(rest, "--source-duration")
        .unwrap_or("0")
        .parse()
        .unwrap_or(-1.0);
    if in_point < 0.0 || source_duration < 0.0 || duration_beats <= 0.0 {
        eprintln!(
            "error: --in-point / --source-duration must be non-negative, --duration-beats positive"
        );
        return ExitCode::from(2);
    }

    let mut root = match read_raw_json(path).and_then(|j| parse_root(&j)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let Some(layers) = root
        .pointer_mut("/timeline/layers")
        .and_then(|v| v.as_array_mut())
    else {
        eprintln!("error: project has no /timeline/layers array");
        return ExitCode::from(2);
    };
    let Some(layer) = layers
        .iter_mut()
        .find(|l| l.get("name").and_then(|n| n.as_str()) == Some(layer_name))
    else {
        eprintln!("error: no layer named '{layer_name}'");
        return ExitCode::FAILURE;
    };
    // LayerType::Audio serializes as 3 (or the string "Audio" historically).
    let is_audio = match layer.get("layerType") {
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(3),
        Some(serde_json::Value::String(s)) => s == "Audio",
        _ => false,
    };
    if !is_audio {
        eprintln!("error: layer '{layer_name}' is not an Audio layer");
        return ExitCode::FAILURE;
    }

    // Write-time non-overlap is a Layer invariant; this tool refuses rather
    // than trims (interactive overlap resolution is the app's job).
    let end_beat = start_beat + duration_beats;
    if let Some(clips) = layer.get("clips").and_then(|c| c.as_array()) {
        for clip in clips {
            let s = clip.get("startBeat").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let d = clip
                .get("durationBeats")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if start_beat < s + d && s < end_beat {
                eprintln!(
                    "error: overlaps existing clip at beats {s}..{} on '{layer_name}'",
                    s + d
                );
                return ExitCode::FAILURE;
            }
        }
    }

    // Serialize a real TimelineClip so the JSON shape (and a freshly minted
    // id) come from the model, not hand-written keys.
    let clip = TimelineClip::new_audio(
        audio_path.to_string(),
        Beats(start_beat),
        Beats(duration_beats),
        Seconds(in_point),
        Seconds(source_duration),
    );
    let clip_id = clip.id.to_string();
    let clip_value = match serde_json::to_value(&clip) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: serialize clip failed: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(clips) = layer.get_mut("clips").and_then(|c| c.as_array_mut()) else {
        eprintln!("error: layer '{layer_name}' has no clips array");
        return ExitCode::from(2);
    };
    clips.push(clip_value);

    println!("added audio clip {clip_id} on '{layer_name}': beats {start_beat}..{end_beat} ({audio_path})");
    validate_and_save(&root, path)
}
