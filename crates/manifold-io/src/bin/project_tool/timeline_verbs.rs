//! Timeline-authoring verbs: `project new`, `layer add`, `clip add-bulk`,
//! `clip list`, `marker add-bulk`, `marker list`.
//!
//! Mutations follow the file-level rule of `project_tool.rs`: raw-JSON surgery
//! on the keys they own, the typed loader as validation gate, the app's own
//! archive writer. Every new entity is built through its typed constructor and
//! serialized, so the JSON shape comes from the model. stdout carries only the
//! payload (an id, or a JSON array); status and warnings go to stderr.
//!
//! IDs are minted unique against the whole file: `short_id` is 32 random bits,
//! so tens of thousands of clips would otherwise collide with real odds.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use serde::Deserialize;
use serde_json::{Value, json};

use manifold_core::clip::TimelineClip;
use manifold_core::color::Color;
use manifold_core::id::{ClipId, LayerId, MarkerId};
use manifold_core::layer::Layer;
use manifold_core::marker::TimelineMarker;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
use manifold_core::types::{LayerType, MarkerColor};
use manifold_core::units::{Beats, Bpm, Seconds};
use manifold_io::loader::load_project_from_json;

use super::{flag_value, parse_root, read_raw_json, tempo_context_from_root, write_validated};

/// Exit 2 for usage / IO / parse, exit 1 for a refused edit or failed lookup.
pub(crate) enum Fail {
    Usage(String),
    Invalid(String),
}

impl Fail {
    fn exit(self) -> ExitCode {
        match self {
            Fail::Usage(m) => {
                eprintln!("error: {m}");
                ExitCode::from(2)
            }
            Fail::Invalid(m) => {
                eprintln!("error: {m}");
                ExitCode::FAILURE
            }
        }
    }
}

fn finish(result: Result<(), Fail>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => f.exit(),
    }
}

/// The first argument as the file path, and every `--flag` checked against the
/// verb's known flags so a typo fails instead of being ignored.
fn file_and_flags<'a>(rest: &'a [String], valued: &[&str], switches: &[&str]) -> Result<&'a str, Fail> {
    let Some(path) = rest.first().filter(|a| !a.starts_with("--")) else {
        return Err(Fail::Usage("missing <file.manifold>".into()));
    };
    let mut i = 1;
    while i < rest.len() {
        let a = rest[i].as_str();
        if valued.contains(&a) {
            if rest.get(i + 1).is_none() {
                return Err(Fail::Usage(format!("{a} needs a value")));
            }
            i += 2;
        } else if switches.contains(&a) {
            i += 1;
        } else {
            return Err(Fail::Usage(format!("unexpected argument '{a}'")));
        }
    }
    Ok(path)
}

fn load_root(path: &str) -> Result<Value, Fail> {
    read_raw_json(path).and_then(|j| parse_root(&j)).map_err(Fail::Usage)
}

fn save_root(root: &Value, path: &str) -> Result<(), Fail> {
    write_validated(root, path).map_err(|(code, m)| if code == 1 { Fail::Invalid(m) } else { Fail::Usage(m) })?;
    eprintln!("saved: {path}");
    Ok(())
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, Fail> {
    let text = std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("failed to read '{path}': {e}")))?;
    serde_json::from_str(&text).map_err(|e| Fail::Usage(format!("failed to parse '{path}': {e}")))
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_else(|_| "null".into()));
}

// ── colours ─────────────────────────────────────────────────────────────

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The marker flag colours as the timeline draws them
/// (`manifold-ui/src/color.rs` `MARKER_*`; `io` cannot depend on `ui`).
const MARKER_RGB: [(MarkerColor, [u8; 3]); 8] = [
    (MarkerColor::Red, [217, 64, 56]),
    (MarkerColor::Orange, [230, 150, 50]),
    (MarkerColor::Yellow, [230, 210, 60]),
    (MarkerColor::Green, [80, 190, 80]),
    (MarkerColor::Cyan, [70, 200, 210]),
    (MarkerColor::Blue, [89, 148, 235]),
    (MarkerColor::Purple, [170, 100, 230]),
    (MarkerColor::White, [220, 220, 225]),
];

/// Markers have an eight-colour palette: a palette name, or `#rrggbb` snapped
/// to the nearest flag colour.
fn marker_color(s: &str) -> Result<MarkerColor, String> {
    if let Some(c) = MarkerColor::ALL.iter().find(|c| c.display_name().eq_ignore_ascii_case(s)) {
        return Ok(*c);
    }
    let rgb = parse_hex(s).ok_or_else(|| format!("marker color '{s}' is neither #rrggbb nor a palette name"))?;
    let dist = |p: [u8; 3]| (0..3).map(|i| (rgb[i] as i32 - p[i] as i32).pow(2)).sum::<i32>();
    Ok(MARKER_RGB.iter().min_by_key(|(_, p)| dist(*p)).map(|(c, _)| *c).unwrap_or_default())
}

// ── shared raw-JSON access ──────────────────────────────────────────────

fn layers_mut(root: &mut Value) -> Result<&mut Vec<Value>, String> {
    root.pointer_mut("/timeline/layers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "project has no /timeline/layers array".to_string())
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The index of the one layer named `name`.
fn layer_index(layers: &[Value], name: &str) -> Result<usize, String> {
    let mut hits = layers.iter().enumerate().filter(|(_, l)| str_at(l, "name") == name).map(|(i, _)| i);
    match (hits.next(), hits.next()) {
        (Some(i), None) => Ok(i),
        (None, _) => Err(format!("no layer named '{name}'")),
        (Some(_), Some(_)) => Err(format!("more than one layer is named '{name}'")),
    }
}

fn layer_type_of(layer: &Value) -> LayerType {
    layer.get("layerType").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn ids_under(root: &Value, pointer: &str, key: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(items) = root.pointer(pointer).and_then(Value::as_array) {
        out.extend(items.iter().map(|v| str_at(v, key).to_string()));
    }
    out
}

fn all_clip_ids(root: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(layers) = root.pointer("/timeline/layers").and_then(Value::as_array) {
        for l in layers {
            if let Some(clips) = l.get("clips").and_then(Value::as_array) {
                out.extend(clips.iter().map(|c| str_at(c, "id").to_string()));
            }
        }
    }
    out
}

/// A fresh `short_id` not in `taken` (which then holds it).
fn mint(taken: &mut HashSet<String>) -> String {
    loop {
        let id = manifold_core::math::short_id();
        if taken.insert(id.clone()) {
            return id;
        }
    }
}

// ── project new ─────────────────────────────────────────────────────────

pub(crate) fn run_project(rest: &[String]) -> ExitCode {
    match rest.split_first() {
        Some((sub, rest)) if sub == "new" => finish(project_new(rest)),
        _ => finish(Err(Fail::Usage("project subcommand required: new".into()))),
    }
}

fn project_new(rest: &[String]) -> Result<(), Fail> {
    let path = file_and_flags(rest, &["--bpm"], &[])?;
    if Path::new(path).exists() {
        return Err(Fail::Invalid(format!("'{path}' exists; refusing to overwrite")));
    }
    let mut project = Project {
        project_name: Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        ..Default::default()
    };
    if let Some(raw) = flag_value(rest, "--bpm") {
        let bpm: f32 = raw.parse().map_err(|_| Fail::Usage(format!("--bpm '{raw}' is not a number")))?;
        if !(20.0..=300.0).contains(&bpm) {
            return Err(Fail::Invalid(format!("bpm {bpm} outside MANIFOLD's 20-300 range")));
        }
        project.settings.bpm = Bpm(bpm);
    }
    manifold_io::saver::save_project(&mut project, Path::new(path), Some("project_tool"), false)
        .map_err(|e| Fail::Usage(format!("failed to save '{path}': {e}")))?;
    eprintln!("saved: {path}");
    Ok(())
}

// ── layer add ───────────────────────────────────────────────────────────

pub(crate) fn run_layer(rest: &[String]) -> ExitCode {
    match rest.split_first() {
        Some((sub, rest)) if sub == "add" => finish(layer_add(rest)),
        _ => finish(Err(Fail::Usage("layer subcommand required: add".into()))),
    }
}

pub(crate) struct NewLayer<'a> {
    pub name: &'a str,
    pub layer_type: LayerType,
    pub parent: Option<&'a str>,
    pub color: Option<[u8; 3]>,
    pub collapsed: bool,
}

fn layer_add(rest: &[String]) -> Result<(), Fail> {
    let path = file_and_flags(rest, &["--name", "--type", "--parent", "--color"], &["--collapsed"])?;
    let (Some(name), Some(kind)) = (flag_value(rest, "--name"), flag_value(rest, "--type")) else {
        return Err(Fail::Usage("layer add needs --name <N> --type audio|trigger|group|generator".into()));
    };
    let layer_type = match kind {
        "audio" => LayerType::Audio,
        "trigger" => LayerType::Trigger,
        "group" => LayerType::Group,
        "generator" => LayerType::Generator,
        other => return Err(Fail::Usage(format!("--type '{other}': want audio|trigger|group|generator"))),
    };
    let color = flag_value(rest, "--color")
        .map(|c| parse_hex(c).ok_or_else(|| Fail::Usage(format!("--color '{c}' is not #rrggbb"))))
        .transpose()?;
    let spec = NewLayer {
        name,
        layer_type,
        parent: flag_value(rest, "--parent"),
        color,
        collapsed: rest.iter().any(|a| a == "--collapsed"),
    };
    let mut root = load_root(path)?;
    let id = add_layer(&mut root, &spec).map_err(Fail::Invalid)?;
    save_root(&root, path)?;
    println!("{id}");
    Ok(())
}

/// Inserts the layer and returns its id. A child goes after its parent's
/// existing subtree, so siblings keep the order they were added in; the app's
/// own "Add Trigger Lane" inserts directly under the owner instead.
pub(crate) fn add_layer(root: &mut Value, spec: &NewLayer<'_>) -> Result<String, String> {
    if spec.name.is_empty() {
        return Err("layer name must not be empty".into());
    }
    let layers = layers_mut(root)?;
    if layers.iter().any(|l| str_at(l, "name") == spec.name) {
        return Err(format!("a layer named '{}' exists (layers are addressed by name)", spec.name));
    }
    let parent = match spec.parent {
        Some(p) => {
            let i = layer_index(layers, p)?;
            let parent_type = layer_type_of(&layers[i]);
            if !parent_type.accepts_child(spec.layer_type) {
                return Err(format!("a {:?} layer cannot be a child of '{p}' ({parent_type:?})", spec.layer_type));
            }
            Some((i, str_at(&layers[i], "layerId").to_string()))
        }
        None if spec.layer_type == LayerType::Trigger => {
            return Err("a trigger lane needs --parent <owner layer>".into());
        }
        None => None,
    };

    let mut layer = match spec.layer_type {
        LayerType::Generator => Layer::new_generator(spec.name.to_string(), PresetTypeId::NONE, 0),
        t => Layer::new(spec.name.to_string(), t, 0),
    };
    let mut taken: HashSet<String> = layers.iter().map(|l| str_at(l, "layerId").to_string()).collect();
    layer.layer_id = LayerId::new(mint(&mut taken));
    layer.parent_layer_id = parent.as_ref().map(|(_, id)| LayerId::new(id));
    layer.layer_color = match spec.color {
        Some([r, g, b]) => Color::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0),
        None => Layer::generate_layer_color(layers.len()),
    };
    layer.is_collapsed = spec.collapsed;
    let id = layer.layer_id.to_string();
    let value = serde_json::to_value(&layer).map_err(|e| format!("serialize layer failed: {e}"))?;

    let at = match &parent {
        Some((pi, pid)) => last_descendant(layers, *pi, pid) + 1,
        None => layers.len(),
    };
    layers.insert(at, value);
    for (i, l) in layers.iter_mut().enumerate() {
        l["index"] = json!(i);
    }
    Ok(id)
}

/// The last flat index holding `parent_id` or one of its descendants.
fn last_descendant(layers: &[Value], parent_index: usize, parent_id: &str) -> usize {
    let parent_of = |id: &str| {
        layers.iter().find(|l| str_at(l, "layerId") == id).and_then(|l| l.get("parentLayerId")).and_then(Value::as_str)
    };
    let descends = |l: &Value| {
        let mut cur = l.get("parentLayerId").and_then(Value::as_str);
        for _ in 0..layers.len() {
            match cur {
                Some(id) if id == parent_id => return true,
                Some(id) => cur = parent_of(id),
                None => return false,
            }
        }
        false
    };
    layers.iter().enumerate().filter(|(_, l)| descends(l)).map(|(i, _)| i).fold(parent_index, usize::max)
}

// ── clip add-bulk ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipSpec {
    pub start_beat: f64,
    pub duration_beats: f64,
    /// Accepted for the builder's convenience; `TimelineClip` has no name field.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub in_point: Option<f64>,
    #[serde(default)]
    pub source_duration: Option<f64>,
}

pub(crate) fn run_clip_bulk(rest: &[String]) -> ExitCode {
    finish(clip_add_bulk(rest))
}

fn clip_add_bulk(rest: &[String]) -> Result<(), Fail> {
    let path = file_and_flags(rest, &["--layer", "--json"], &[])?;
    let (Some(layer), Some(json_path)) = (flag_value(rest, "--layer"), flag_value(rest, "--json")) else {
        return Err(Fail::Usage("clip add-bulk needs --layer <name> --json <clips.json>".into()));
    };
    let specs: Vec<ClipSpec> = read_json_file(json_path)?;
    let named = specs.iter().filter(|s| s.name.is_some()).count();
    if named > 0 {
        eprintln!("note: {named} clip name(s) not stored (a timeline clip has no name field)");
    }
    let mut root = load_root(path)?;
    let ids = add_clips(&mut root, layer, &specs).map_err(Fail::Invalid)?;
    save_root(&root, path)?;
    print_json(&json!(ids));
    Ok(())
}

/// Adds every clip or none. Returns the new ids in input order.
pub(crate) fn add_clips(root: &mut Value, layer_name: &str, specs: &[ClipSpec]) -> Result<Vec<String>, String> {
    for (i, s) in specs.iter().enumerate() {
        let ok = s.start_beat.is_finite() && s.start_beat >= 0.0 && s.duration_beats.is_finite() && s.duration_beats > 0.0;
        if !ok {
            return Err(format!("clip #{i}: start_beat must be >= 0 and duration_beats > 0 (got {} / {})", s.start_beat, s.duration_beats));
        }
        for (what, v) in [("in_point", s.in_point), ("source_duration", s.source_duration)] {
            if v.is_some_and(|v| !v.is_finite() || v < 0.0) {
                return Err(format!("clip #{i}: {what} must be a non-negative number"));
            }
        }
    }
    let mut taken = all_clip_ids(root);
    let layers = layers_mut(root)?;
    let li = layer_index(layers, layer_name)?;
    let layer = &mut layers[li];
    let layer_type = layer_type_of(layer);
    let make: fn(&ClipSpec) -> Result<TimelineClip, String> = match layer_type {
        LayerType::Audio => |s| {
            let path = s.path.as_deref().filter(|p| !p.is_empty()).ok_or("an audio clip needs a path")?;
            Ok(TimelineClip::new_audio(
                path.to_string(),
                Beats(s.start_beat),
                Beats(s.duration_beats),
                Seconds(s.in_point.unwrap_or(0.0)),
                Seconds(s.source_duration.unwrap_or(0.0)),
            ))
        },
        LayerType::Trigger => |s| Ok(TimelineClip::new_trigger(Beats(s.start_beat), Beats(s.duration_beats))),
        LayerType::Generator | LayerType::Dmx => {
            |s| Ok(TimelineClip::new_generator(Beats(s.start_beat), Beats(s.duration_beats)))
        }
        t => return Err(format!("layer '{layer_name}' is a {t:?} layer; add-bulk takes audio, trigger or generator layers")),
    };

    // Overlap: every interval, existing and new, sorted by start; a start
    // before the furthest end so far overlaps that end's owner. Touching is fine.
    let existing = layer.get("clips").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut spans: Vec<(f64, f64, String)> = existing
        .iter()
        .map(|c| {
            let s = c.get("startBeat").and_then(Value::as_f64).unwrap_or(0.0);
            let d = c.get("durationBeats").and_then(Value::as_f64).unwrap_or(1.0);
            (s, s + d, format!("existing clip {}", str_at(c, "id")))
        })
        .collect();
    spans.extend(specs.iter().enumerate().map(|(i, s)| (s.start_beat, s.start_beat + s.duration_beats, format!("input clip #{i}"))));
    spans.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut reach: Option<usize> = None;
    for (i, span) in spans.iter().enumerate() {
        if let Some(r) = reach {
            let (s0, e0, who) = &spans[r];
            if span.0 < *e0 {
                return Err(format!(
                    "{who} (beats {s0}..{e0}) overlaps {} (beats {}..{}) on layer '{layer_name}'; nothing written",
                    span.2, span.0, span.1
                ));
            }
        }
        if reach.is_none_or(|r| span.1 > spans[r].1) {
            reach = Some(i);
        }
    }

    let mut ids = Vec::with_capacity(specs.len());
    let mut values = Vec::with_capacity(specs.len());
    for (i, s) in specs.iter().enumerate() {
        let mut clip = make(s).map_err(|e| format!("clip #{i}: {e}"))?;
        clip.id = ClipId::new(mint(&mut taken));
        ids.push(clip.id.to_string());
        values.push(serde_json::to_value(&clip).map_err(|e| format!("serialize clip failed: {e}"))?);
    }
    if layer.get("clips").is_none_or(|c| !c.is_array()) {
        layer["clips"] = json!([]);
    }
    let clips = layer["clips"].as_array_mut().ok_or("clips is not an array")?;
    clips.extend(values);
    clips.sort_by(|a, b| {
        let s = |v: &Value| v.get("startBeat").and_then(Value::as_f64).unwrap_or(0.0);
        s(a).total_cmp(&s(b))
    });
    Ok(ids)
}

// ── marker add-bulk ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarkerSpec {
    pub beat: f64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

pub(crate) fn run_marker(rest: &[String]) -> ExitCode {
    match rest.split_first() {
        Some((sub, rest)) if sub == "add-bulk" => finish(marker_add_bulk(rest)),
        Some((sub, rest)) if sub == "list" => finish(marker_list(rest)),
        _ => finish(Err(Fail::Usage("marker subcommand required: add-bulk | list".into()))),
    }
}

fn marker_add_bulk(rest: &[String]) -> Result<(), Fail> {
    let path = file_and_flags(rest, &["--json"], &[])?;
    let Some(json_path) = flag_value(rest, "--json") else {
        return Err(Fail::Usage("marker add-bulk needs --json <markers.json>".into()));
    };
    let specs: Vec<MarkerSpec> = read_json_file(json_path)?;
    let mut root = load_root(path)?;
    let ids = add_markers(&mut root, &specs).map_err(Fail::Invalid)?;
    save_root(&root, path)?;
    print_json(&json!(ids));
    Ok(())
}

/// Adds every marker or none, kept sorted by beat. Returns ids in input order.
pub(crate) fn add_markers(root: &mut Value, specs: &[MarkerSpec]) -> Result<Vec<String>, String> {
    let mut taken = ids_under(root, "/timeline/markers", "id");
    let mut ids = Vec::with_capacity(specs.len());
    let mut values = Vec::with_capacity(specs.len());
    for (i, s) in specs.iter().enumerate() {
        if !s.beat.is_finite() || s.beat < 0.0 {
            return Err(format!("marker #{i}: beat must be >= 0 (got {})", s.beat));
        }
        let mut marker = TimelineMarker::new(Beats(s.beat)).with_name(s.name.clone());
        if let Some(c) = &s.color {
            marker = marker.with_color(marker_color(c).map_err(|e| format!("marker #{i}: {e}"))?);
        }
        marker.id = MarkerId::new(mint(&mut taken));
        ids.push(marker.id.to_string());
        values.push(serde_json::to_value(&marker).map_err(|e| format!("serialize marker failed: {e}"))?);
    }
    let timeline = root.get_mut("timeline").filter(|t| t.is_object()).ok_or("project has no timeline object")?;
    if timeline.get("markers").is_none_or(|m| !m.is_array()) {
        timeline["markers"] = json!([]);
    }
    let markers = timeline["markers"].as_array_mut().ok_or("markers is not an array")?;
    markers.extend(values);
    markers.sort_by(|a, b| {
        let beat = |v: &Value| v.get("beat").and_then(Value::as_f64).unwrap_or(0.0);
        beat(a).total_cmp(&beat(b))
    });
    Ok(ids)
}

// ── lists ───────────────────────────────────────────────────────────────

/// The project as the app sees it after load, plus the beat→seconds context
/// `tempo at` uses.
fn load_for_listing(rest: &[String]) -> Result<(Project, impl Fn(f64) -> f64), Fail> {
    let path = file_and_flags(rest, &[], &["--json"])?;
    let json = read_raw_json(path).map_err(Fail::Usage)?;
    let root = parse_root(&json).map_err(Fail::Usage)?;
    let (mut map, fallback) = tempo_context_from_root(&root).map_err(Fail::Usage)?;
    map.ensure_valid();
    let project = load_project_from_json(&json).map_err(|e| Fail::Usage(format!("failed to load '{path}': {e}")))?;
    let seconds = move |beat: f64| TempoMapConverter::beat_to_seconds_immut(&map, Beats(beat), fallback).0;
    Ok((project, seconds))
}

pub(crate) fn run_clip_list(rest: &[String]) -> ExitCode {
    finish(clip_list(rest))
}

fn clip_list(rest: &[String]) -> Result<(), Fail> {
    let (project, seconds) = load_for_listing(rest)?;
    let mut rows = Vec::with_capacity(project.total_clip_count());
    for layer in &project.timeline.layers {
        let mut clips: Vec<&TimelineClip> = layer.clips.iter().collect();
        clips.sort_by(|a, b| a.start_beat.0.total_cmp(&b.start_beat.0));
        for c in clips {
            // Clips carry no name; an audio clip shows its file name.
            let name = Path::new(&c.audio_file_path).file_name().map(|n| n.to_string_lossy().into_owned());
            rows.push(json!({
                "layer": layer.name,
                "layer_id": layer.layer_id.to_string(),
                "clip_id": c.id.to_string(),
                "start_beat": c.start_beat.0,
                "duration_beats": c.duration_beats.0,
                "start_s": seconds(c.start_beat.0),
                "end_s": seconds(c.end_beat().0),
                "name": name,
            }));
        }
    }
    print_json(&Value::Array(rows));
    Ok(())
}

fn marker_list(rest: &[String]) -> Result<(), Fail> {
    let (project, seconds) = load_for_listing(rest)?;
    let rows: Vec<Value> = project
        .timeline
        .markers
        .iter()
        .map(|m| json!({"marker_id": m.id.to_string(), "beat": m.beat.0, "seconds": seconds(m.beat.0), "name": m.name}))
        .collect();
    print_json(&Value::Array(rows));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_root() -> Value {
        serde_json::to_value(Project::default()).unwrap()
    }

    fn layer<'a>(name: &'a str, layer_type: LayerType, parent: Option<&'a str>) -> NewLayer<'a> {
        NewLayer { name, layer_type, parent, color: None, collapsed: false }
    }

    fn names(root: &Value) -> Vec<String> {
        root["timeline"]["layers"].as_array().unwrap().iter().map(|l| str_at(l, "name").to_string()).collect()
    }

    fn clip(start: f64, dur: f64) -> ClipSpec {
        ClipSpec { start_beat: start, duration_beats: dur, name: None, path: None, in_point: None, source_duration: None }
    }

    #[test]
    fn children_land_after_their_parents_subtree_in_add_order() {
        let mut root = empty_root();
        add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("Drums", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("Kick labels", LayerType::Trigger, Some("Mix"))).unwrap();
        add_layer(&mut root, &layer("Snare labels", LayerType::Trigger, Some("Mix"))).unwrap();
        assert_eq!(names(&root), ["Mix", "Kick labels", "Snare labels", "Drums"]);
        let idx: Vec<i64> = root["timeline"]["layers"].as_array().unwrap().iter().map(|l| l["index"].as_i64().unwrap()).collect();
        assert_eq!(idx, [0, 1, 2, 3]);
    }

    #[test]
    fn layer_rules_refuse_bad_parents_and_duplicates() {
        let mut root = empty_root();
        add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("Lane", LayerType::Trigger, Some("Mix"))).unwrap();
        assert!(add_layer(&mut root, &layer("Orphan", LayerType::Trigger, None)).is_err());
        assert!(add_layer(&mut root, &layer("Nested", LayerType::Trigger, Some("Lane"))).is_err());
        assert!(add_layer(&mut root, &layer("Under audio", LayerType::Audio, Some("Mix"))).is_err());
        assert!(add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).is_err());
        assert!(add_layer(&mut root, &layer("Ghost child", LayerType::Audio, Some("Nope"))).is_err());
    }

    #[test]
    fn overlap_sweep_names_the_first_pair_and_allows_touching() {
        let mut root = empty_root();
        add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("Hits", LayerType::Trigger, Some("Mix"))).unwrap();
        let ids = add_clips(&mut root, "Hits", &[clip(4.0, 1.0), clip(0.0, 4.0), clip(5.0, 0.5)]).unwrap();
        assert_eq!(ids.len(), 3);
        // A long clip covering two later ones still overlaps the first it reaches.
        let err = add_clips(&mut root, "Hits", &[clip(10.0, 8.0), clip(12.0, 1.0)]).unwrap_err();
        assert!(err.contains("input clip #0") && err.contains("input clip #1"), "{err}");
        let err = add_clips(&mut root, "Hits", &[clip(4.5, 0.25)]).unwrap_err();
        assert!(err.contains("existing clip"), "{err}");
        assert_eq!(root["timeline"]["layers"][1]["clips"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn clip_ids_come_back_in_input_order() {
        let mut root = empty_root();
        add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("Hits", LayerType::Trigger, Some("Mix"))).unwrap();
        let ids = add_clips(&mut root, "Hits", &[clip(8.0, 1.0), clip(0.0, 1.0), clip(4.0, 1.0)]).unwrap();
        let clips = root["timeline"]["layers"][1]["clips"].as_array().unwrap();
        let by_id = |id: &str| clips.iter().find(|c| str_at(c, "id") == id).unwrap()["startBeat"].as_f64().unwrap();
        assert_eq!(ids.iter().map(|i| by_id(i)).collect::<Vec<_>>(), [8.0, 0.0, 4.0]);
        assert_eq!(clips.iter().map(|c| c["startBeat"].as_f64().unwrap()).collect::<Vec<_>>(), [0.0, 4.0, 8.0]);
    }

    #[test]
    fn audio_clips_need_a_path_and_groups_take_none() {
        let mut root = empty_root();
        add_layer(&mut root, &layer("Mix", LayerType::Audio, None)).unwrap();
        add_layer(&mut root, &layer("G", LayerType::Group, None)).unwrap();
        assert!(add_clips(&mut root, "Mix", &[clip(0.0, 1.0)]).is_err());
        assert!(add_clips(&mut root, "G", &[clip(0.0, 1.0)]).is_err());
    }

    #[test]
    fn marker_colors_snap_to_the_palette() {
        assert_eq!(marker_color("#ff0000").unwrap(), MarkerColor::Red);
        assert_eq!(marker_color("#00ff00").unwrap(), MarkerColor::Green);
        assert_eq!(marker_color("cyan").unwrap(), MarkerColor::Cyan);
        assert!(marker_color("#12345").is_err());
    }
}
