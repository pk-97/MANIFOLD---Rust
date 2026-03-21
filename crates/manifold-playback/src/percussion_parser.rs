// Port of Unity JsonPercussionAnalysisParser.cs (440 lines).
// JSON parser for imported percussion detection files.
// Supports either object-root ({ events: [...] }) or array-root ([...]).

use manifold_core::percussion_analysis::{
    PercussionAnalysisData, PercussionBeatGrid, PercussionEvent, PercussionTriggerType,
};
use serde::Deserialize;

// ─── Trait ───

/// Port of Unity IPercussionAnalysisParser interface.
pub trait PercussionAnalysisParser {
    fn try_parse(&self, raw_json: &str) -> Result<PercussionAnalysisData, String>;
}

// ─── JsonPercussionAnalysisParser ───

/// Port of Unity JsonPercussionAnalysisParser class.
pub struct JsonPercussionAnalysisParser;

impl PercussionAnalysisParser for JsonPercussionAnalysisParser {
    fn try_parse(&self, raw_json: &str) -> Result<PercussionAnalysisData, String> {
        if raw_json.trim().is_empty() {
            return Err("Input JSON is empty.".to_string());
        }

        let normalized = normalize_root(raw_json);

        let root: RootDto = serde_json::from_str(&normalized)
            .map_err(|ex| format!("JSON parse error: {}", ex))?;

        let source_events = if !root.events.is_empty() {
            &root.events[..]
        } else {
            &root.triggers[..]
        };

        let mut events: Vec<PercussionEvent> = Vec::with_capacity(source_events.len());
        for dto in source_events.iter() {
            if let Some(parsed) = try_convert_event(dto) {
                events.push(parsed);
            }
        }

        let track_id = first_non_empty(&[
            root.track_id.as_deref().unwrap_or(""),
            root.track_id_upper.as_deref().unwrap_or(""),
            root.id.as_deref().unwrap_or(""),
            "Imported Track",
        ]);
        let (beat_grid, grid_bpm, grid_confidence) = parse_beat_grid(&root);
        let bpm = first_finite_non_negative(&[root.bpm, root.tempo, grid_bpm]);
        let bpm_confidence = first_finite_non_negative(&[
            root.bpm_confidence,
            root.tempo_confidence,
            root.confidence,
            grid_confidence,
        ]);

        // Allow BPM-only results (no events) when beat grid or BPM is present
        if events.is_empty() && beat_grid.is_none() && !(bpm.is_finite() && bpm > 0.0) {
            return Err("No events, beat grid, or BPM data found in JSON.".to_string());
        }

        let envelope_values = root
            .energy_envelope
            .as_ref()
            .filter(|e| !e.values.is_empty())
            .map(|e| e.values.clone());

        let mut analysis = PercussionAnalysisData::new(
            &track_id,
            bpm,
            events,
            bpm_confidence,
            beat_grid,
            envelope_values,
        );
        analysis.ensure_valid();
        Ok(analysis)
    }
}

// ─── NormalizeRoot ───

fn normalize_root(raw_json: &str) -> String {
    let trimmed = raw_json.trim();
    if trimmed.starts_with('[') {
        format!("{{\"events\":{}}}", trimmed)
    } else {
        trimmed.to_string()
    }
}

// ─── TryConvertEvent ───

fn try_convert_event(dto: &EventDto) -> Option<PercussionEvent> {
    let raw_type = first_non_empty(&[
        dto.r#type.as_deref().unwrap_or(""),
        dto.trigger_type.as_deref().unwrap_or(""),
        dto.class.as_deref().unwrap_or(""),
        dto.label.as_deref().unwrap_or(""),
        dto.instrument.as_deref().unwrap_or(""),
    ]);
    let trigger_type = parse_trigger_type(&raw_type);

    let time_seconds =
        first_finite_non_negative(&[dto.time_seconds, dto.time, dto.seconds, dto.timestamp]);
    if !time_seconds.is_finite() {
        return None;
    }

    let mut confidence =
        first_finite_non_negative(&[dto.confidence, dto.strength, dto.velocity]);
    if !confidence.is_finite() {
        confidence = 1.0;
    }

    // Common extractor range: MIDI-like velocity 0..127.
    if confidence > 1.0 && confidence <= 127.0 {
        confidence /= 127.0;
    }

    let duration_seconds =
        first_finite_non_negative(&[dto.duration_seconds, dto.duration, dto.duration_sec]);
    let duration_seconds = if duration_seconds.is_finite() {
        duration_seconds
    } else {
        0.0
    };

    Some(PercussionEvent::new(
        trigger_type,
        time_seconds,
        confidence,
        duration_seconds,
    ))
}

// ─── ParseTriggerType ───

fn parse_trigger_type(raw_type: &str) -> PercussionTriggerType {
    if raw_type.trim().is_empty() {
        return PercussionTriggerType::Unknown;
    }

    let value = raw_type.trim().to_lowercase();

    match value.as_str() {
        "kick" | "bd" | "bassdrum" | "bass_drum" => PercussionTriggerType::Kick,

        "snare" | "sd" | "rimshot" => PercussionTriggerType::Snare,

        "clap" | "handclap" | "hand_clap" => PercussionTriggerType::Clap,

        "hat" | "hihat" | "hi_hat" | "hh" | "cymbal" => PercussionTriggerType::Hat,

        "perc" | "percussion" | "tom" | "shaker" | "rim" => PercussionTriggerType::Perc,

        "bass_sustained" | "bass_long" | "bass_reese" | "reese" => {
            PercussionTriggerType::BassSustained
        }

        "bass" | "sub" | "subbass" | "bassline" | "wobble" | "growl" | "roar" | "siren"
        | "stab" => PercussionTriggerType::Bass,

        "synth" | "lead" | "arp" | "pluck" | "chord" => PercussionTriggerType::Synth,

        "pad" | "drone" | "atmosphere" | "ambient" => PercussionTriggerType::Pad,

        "vocal" | "vocals" | "voice" | "vox" | "singing" => PercussionTriggerType::Vocal,

        _ => {
            if value.contains("kick") {
                return PercussionTriggerType::Kick;
            }
            if value.contains("snare") {
                return PercussionTriggerType::Snare;
            }
            if value.contains("clap") {
                return PercussionTriggerType::Clap;
            }
            if value.contains("hat") || value.contains("cymbal") {
                return PercussionTriggerType::Hat;
            }
            if value.contains("perc") || value.contains("tom") || value.contains("shaker") {
                return PercussionTriggerType::Perc;
            }
            if value.contains("bassdrum") {
                return PercussionTriggerType::Kick;
            }
            if value.contains("pad")
                || value.contains("drone")
                || value.contains("atmosphere")
                || value.contains("ambient")
            {
                return PercussionTriggerType::Pad;
            }
            if value.contains("synth")
                || value.contains("lead")
                || value.contains("arp")
                || value.contains("pluck")
                || value.contains("chord")
            {
                return PercussionTriggerType::Synth;
            }
            if value.contains("bass_sustained")
                || value.contains("bass_long")
                || value.contains("bass_reese")
            {
                return PercussionTriggerType::BassSustained;
            }
            if value.contains("bass")
                || value.contains("sub")
                || value.contains("reese")
                || value.contains("wobble")
                || value.contains("growl")
                || value.contains("roar")
                || value.contains("siren")
                || value.contains("stab")
            {
                return PercussionTriggerType::Bass;
            }
            if value.contains("vocal")
                || value.contains("voice")
                || value.contains("vox")
                || value.contains("singing")
            {
                return PercussionTriggerType::Vocal;
            }
            PercussionTriggerType::Unknown
        }
    }
}

// ─── FirstNonEmpty ───

fn first_non_empty(values: &[&str]) -> String {
    for &v in values {
        if !v.trim().is_empty() {
            return v.to_string();
        }
    }
    String::new()
}

// ─── FirstFiniteNonNegative ───

fn first_finite_non_negative(values: &[f32]) -> f32 {
    for &v in values {
        if v.is_finite() && v >= 0.0 {
            return v;
        }
    }
    f32::NAN
}

// ─── ParseBeatGrid ───

fn parse_beat_grid(root: &RootDto) -> (Option<PercussionBeatGrid>, f32, f32) {
    let dto = root
        .beat_grid
        .as_ref()
        .or(root.tempo_grid.as_ref())
        .or(root.grid.as_ref());

    let dto = match dto {
        Some(d) => d,
        None => return (None, f32::NAN, f32::NAN),
    };

    let beat_times_source = first_non_empty_float_array(&[
        &dto.beat_times,
        &dto.beat_times_seconds,
        &dto.beats,
        &dto.times,
    ]);
    let beat_times = convert_finite_non_negative(beat_times_source);
    if beat_times.len() < 2 {
        return (None, f32::NAN, f32::NAN);
    }

    let downbeat_source =
        first_non_empty_int_array(&[&dto.downbeat_indices, &dto.downbeats]);
    let downbeat_indices = convert_non_negative(downbeat_source);

    let grid_bpm = first_finite_non_negative(&[dto.bpm_derived, dto.bpm, dto.tempo]);
    let grid_confidence =
        first_finite_non_negative(&[dto.confidence, dto.bpm_confidence, dto.tempo_confidence]);
    let mode = {
        
        first_non_empty(&[dto.mode.as_deref().unwrap_or(""), "beat_times"])
    };
    let onset_to_peak = first_finite_non_negative(&[dto.onset_to_peak_seconds]);

    let mut beat_grid = PercussionBeatGrid::new(
        &mode,
        beat_times,
        downbeat_indices,
        grid_bpm,
        grid_confidence,
        onset_to_peak,
    );
    beat_grid.ensure_valid();
    if !beat_grid.has_usable_beats() {
        return (None, grid_bpm, grid_confidence);
    }

    (Some(beat_grid), grid_bpm, grid_confidence)
}

// ─── ConvertFiniteNonNegative ───

fn convert_finite_non_negative(values: Option<&[f32]>) -> Vec<f32> {
    let values = match values {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut result = Vec::with_capacity(values.len());
    for &value in values {
        if value.is_finite() && value >= 0.0 {
            result.push(value);
        }
    }
    result
}

// ─── ConvertNonNegative ───

fn convert_non_negative(values: Option<&[i32]>) -> Vec<i32> {
    let values = match values {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut result = Vec::with_capacity(values.len());
    for &value in values {
        if value >= 0 {
            result.push(value);
        }
    }
    result
}

// ─── FirstNonEmptyArray (float overload) ───

fn first_non_empty_float_array<'a>(arrays: &[&'a [f32]]) -> Option<&'a [f32]> {
    arrays.iter().find(|&&arr| !arr.is_empty()).copied().map(|v| v as _)
}

// ─── FirstNonEmptyArray (int overload) ───

fn first_non_empty_int_array<'a>(arrays: &[&'a [i32]]) -> Option<&'a [i32]> {
    arrays.iter().find(|&&arr| !arr.is_empty()).copied().map(|v| v as _)
}

// ─── DTOs ───

fn nan_default() -> f32 {
    f32::NAN
}

fn empty_vec_f32() -> Vec<f32> {
    Vec::new()
}

fn empty_vec_i32() -> Vec<i32> {
    Vec::new()
}

fn empty_vec_event() -> Vec<EventDto> {
    Vec::new()
}

/// Port of Unity RootDto [Serializable] class.
#[derive(Debug, Deserialize)]
struct RootDto {
    #[serde(rename = "trackId")]
    track_id: Option<String>,
    // Unity has both trackId and trackID as separate fields
    #[serde(rename = "trackID")]
    track_id_upper: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default = "nan_default")]
    bpm: f32,
    #[serde(default = "nan_default")]
    tempo: f32,
    #[serde(rename = "bpmConfidence", default = "nan_default")]
    bpm_confidence: f32,
    #[serde(rename = "tempoConfidence", default = "nan_default")]
    tempo_confidence: f32,
    #[serde(default = "nan_default")]
    confidence: f32,
    #[serde(rename = "beatGrid", default)]
    beat_grid: Option<BeatGridDto>,
    #[serde(rename = "tempoGrid", default)]
    tempo_grid: Option<BeatGridDto>,
    #[serde(default)]
    grid: Option<BeatGridDto>,
    #[serde(default = "empty_vec_event")]
    events: Vec<EventDto>,
    #[serde(default = "empty_vec_event")]
    triggers: Vec<EventDto>,
    #[serde(rename = "energyEnvelope", default)]
    energy_envelope: Option<EnergyEnvelopeDto>,
}

/// Port of Unity EnergyEnvelopeDto [Serializable] class.
#[derive(Debug, Deserialize)]
struct EnergyEnvelopeDto {
    #[serde(default, rename = "resolution")]
    _resolution: Option<String>,
    #[serde(default = "empty_vec_f32")]
    values: Vec<f32>,
}

/// Port of Unity BeatGridDto [Serializable] class.
#[derive(Debug, Deserialize)]
struct BeatGridDto {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default = "nan_default")]
    bpm: f32,
    #[serde(rename = "bpmDerived", default = "nan_default")]
    bpm_derived: f32,
    #[serde(default = "nan_default")]
    tempo: f32,
    #[serde(default = "nan_default")]
    confidence: f32,
    #[serde(rename = "bpmConfidence", default = "nan_default")]
    bpm_confidence: f32,
    #[serde(rename = "tempoConfidence", default = "nan_default")]
    tempo_confidence: f32,
    #[serde(rename = "beatTimes", default = "empty_vec_f32")]
    beat_times: Vec<f32>,
    #[serde(rename = "beatTimesSeconds", default = "empty_vec_f32")]
    beat_times_seconds: Vec<f32>,
    #[serde(default = "empty_vec_f32")]
    beats: Vec<f32>,
    #[serde(default = "empty_vec_f32")]
    times: Vec<f32>,
    #[serde(rename = "downbeatIndices", default = "empty_vec_i32")]
    downbeat_indices: Vec<i32>,
    #[serde(default = "empty_vec_i32")]
    downbeats: Vec<i32>,
    #[serde(rename = "onsetToPeakSeconds", default = "nan_default")]
    onset_to_peak_seconds: f32,
}

/// Port of Unity EventDto [Serializable] class.
#[derive(Debug, Deserialize)]
struct EventDto {
    #[serde(rename = "type", default)]
    r#type: Option<String>,
    #[serde(rename = "triggerType", default)]
    trigger_type: Option<String>,
    #[serde(rename = "class", default)]
    class: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    instrument: Option<String>,
    #[serde(default = "nan_default")]
    time: f32,
    #[serde(rename = "timeSeconds", default = "nan_default")]
    time_seconds: f32,
    #[serde(default = "nan_default")]
    seconds: f32,
    #[serde(default = "nan_default")]
    timestamp: f32,
    #[serde(default = "nan_default")]
    confidence: f32,
    #[serde(default = "nan_default")]
    strength: f32,
    #[serde(default = "nan_default")]
    velocity: f32,
    #[serde(rename = "durationSeconds", default = "nan_default")]
    duration_seconds: f32,
    #[serde(default = "nan_default")]
    duration: f32,
    #[serde(rename = "durationSec", default = "nan_default")]
    duration_sec: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trigger_type_exact_matches() {
        assert_eq!(parse_trigger_type("kick"), PercussionTriggerType::Kick);
        assert_eq!(parse_trigger_type("bd"), PercussionTriggerType::Kick);
        assert_eq!(parse_trigger_type("bassdrum"), PercussionTriggerType::Kick);
        assert_eq!(parse_trigger_type("bass_drum"), PercussionTriggerType::Kick);
        assert_eq!(parse_trigger_type("snare"), PercussionTriggerType::Snare);
        assert_eq!(parse_trigger_type("sd"), PercussionTriggerType::Snare);
        assert_eq!(parse_trigger_type("rimshot"), PercussionTriggerType::Snare);
        assert_eq!(parse_trigger_type("clap"), PercussionTriggerType::Clap);
        assert_eq!(parse_trigger_type("handclap"), PercussionTriggerType::Clap);
        assert_eq!(parse_trigger_type("hand_clap"), PercussionTriggerType::Clap);
        assert_eq!(parse_trigger_type("hat"), PercussionTriggerType::Hat);
        assert_eq!(parse_trigger_type("hihat"), PercussionTriggerType::Hat);
        assert_eq!(parse_trigger_type("hi_hat"), PercussionTriggerType::Hat);
        assert_eq!(parse_trigger_type("hh"), PercussionTriggerType::Hat);
        assert_eq!(parse_trigger_type("cymbal"), PercussionTriggerType::Hat);
        assert_eq!(parse_trigger_type("perc"), PercussionTriggerType::Perc);
        assert_eq!(parse_trigger_type("percussion"), PercussionTriggerType::Perc);
        assert_eq!(parse_trigger_type("tom"), PercussionTriggerType::Perc);
        assert_eq!(parse_trigger_type("shaker"), PercussionTriggerType::Perc);
        assert_eq!(parse_trigger_type("rim"), PercussionTriggerType::Perc);
        assert_eq!(
            parse_trigger_type("bass_sustained"),
            PercussionTriggerType::BassSustained
        );
        assert_eq!(
            parse_trigger_type("bass_long"),
            PercussionTriggerType::BassSustained
        );
        assert_eq!(
            parse_trigger_type("bass_reese"),
            PercussionTriggerType::BassSustained
        );
        assert_eq!(
            parse_trigger_type("reese"),
            PercussionTriggerType::BassSustained
        );
        assert_eq!(parse_trigger_type("bass"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("sub"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("subbass"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("bassline"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("wobble"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("growl"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("roar"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("siren"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("stab"), PercussionTriggerType::Bass);
        assert_eq!(parse_trigger_type("synth"), PercussionTriggerType::Synth);
        assert_eq!(parse_trigger_type("lead"), PercussionTriggerType::Synth);
        assert_eq!(parse_trigger_type("arp"), PercussionTriggerType::Synth);
        assert_eq!(parse_trigger_type("pluck"), PercussionTriggerType::Synth);
        assert_eq!(parse_trigger_type("chord"), PercussionTriggerType::Synth);
        assert_eq!(parse_trigger_type("pad"), PercussionTriggerType::Pad);
        assert_eq!(parse_trigger_type("drone"), PercussionTriggerType::Pad);
        assert_eq!(parse_trigger_type("atmosphere"), PercussionTriggerType::Pad);
        assert_eq!(parse_trigger_type("ambient"), PercussionTriggerType::Pad);
        assert_eq!(parse_trigger_type("vocal"), PercussionTriggerType::Vocal);
        assert_eq!(parse_trigger_type("vocals"), PercussionTriggerType::Vocal);
        assert_eq!(parse_trigger_type("voice"), PercussionTriggerType::Vocal);
        assert_eq!(parse_trigger_type("vox"), PercussionTriggerType::Vocal);
        assert_eq!(parse_trigger_type("singing"), PercussionTriggerType::Vocal);
        assert_eq!(parse_trigger_type(""), PercussionTriggerType::Unknown);
        assert_eq!(parse_trigger_type("  "), PercussionTriggerType::Unknown);
    }

    #[test]
    fn test_parse_trigger_type_contains_fallbacks() {
        assert_eq!(parse_trigger_type("kick_drum"), PercussionTriggerType::Kick);
        assert_eq!(
            parse_trigger_type("snare_roll"),
            PercussionTriggerType::Snare
        );
        assert_eq!(
            parse_trigger_type("open_hihat"),
            PercussionTriggerType::Hat
        );
        assert_eq!(
            parse_trigger_type("heavy_bass"),
            PercussionTriggerType::Bass
        );
        assert_eq!(
            parse_trigger_type("poly_synth"),
            PercussionTriggerType::Synth
        );
    }

    #[test]
    fn test_normalize_root_array() {
        let json = r#"[{"time": 1.0}]"#;
        let normalized = normalize_root(json);
        assert!(normalized.starts_with("{\"events\":"));
    }

    #[test]
    fn test_normalize_root_object() {
        let json = r#"{"events": []}"#;
        let normalized = normalize_root(json);
        assert_eq!(normalized, json);
    }

    #[test]
    fn test_first_finite_non_negative() {
        assert_eq!(
            first_finite_non_negative(&[f32::NAN, -1.0, 0.5]),
            0.5
        );
        assert!(first_finite_non_negative(&[f32::NAN, f32::NAN]).is_nan());
        assert_eq!(first_finite_non_negative(&[0.0, 1.0]), 0.0);
    }

    #[test]
    fn test_first_non_empty() {
        assert_eq!(first_non_empty(&["", "  ", "hello"]), "hello");
        assert_eq!(first_non_empty(&["first", "second"]), "first");
        assert_eq!(first_non_empty(&["", ""]), "");
    }

    #[test]
    fn test_parse_object_root() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{
            "trackId": "test_track",
            "bpm": 128.0,
            "events": [
                {"type": "kick", "timeSeconds": 0.0, "confidence": 0.9},
                {"type": "snare", "timeSeconds": 0.46875, "confidence": 0.8}
            ]
        }"#;
        let result = parser.try_parse(json);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data.track_id, "test_track");
        assert_eq!(data.bpm, 128.0);
        assert_eq!(data.events.len(), 2);
        assert_eq!(data.events[0].trigger_type, PercussionTriggerType::Kick);
        assert_eq!(data.events[1].trigger_type, PercussionTriggerType::Snare);
    }

    #[test]
    fn test_parse_array_root() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"[
            {"type": "kick", "timeSeconds": 0.0, "confidence": 0.9},
            {"type": "hat", "time": 0.25, "confidence": 0.7}
        ]"#;
        let result = parser.try_parse(json);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data.events.len(), 2);
    }

    #[test]
    fn test_parse_empty_json_fails() {
        let parser = JsonPercussionAnalysisParser;
        assert!(parser.try_parse("").is_err());
        assert!(parser.try_parse("  ").is_err());
    }

    #[test]
    fn test_parse_no_events_no_bpm_fails() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{"trackId": "t"}"#;
        let result = parser.try_parse(json);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("No events, beat grid, or BPM data found"));
    }

    #[test]
    fn test_midi_velocity_normalization() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{
            "bpm": 120.0,
            "events": [{"type": "kick", "timeSeconds": 0.0, "velocity": 100.0}]
        }"#;
        let result = parser.try_parse(json).unwrap();
        let confidence = result.events[0].confidence;
        // 100 / 127 ≈ 0.787
        assert!((confidence - 100.0 / 127.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_bpm_only() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{"bpm": 140.0}"#;
        let result = parser.try_parse(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().bpm, 140.0);
    }

    #[test]
    fn test_parse_beat_grid() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{
            "bpm": 120.0,
            "beatGrid": {
                "beatTimes": [0.0, 0.5, 1.0, 1.5, 2.0],
                "bpm": 120.0,
                "confidence": 0.95
            }
        }"#;
        let result = parser.try_parse(json).unwrap();
        assert!(result.beat_grid.is_some());
        let grid = result.beat_grid.unwrap();
        assert_eq!(grid.beat_times_seconds.len(), 5);
        assert!((grid.bpm_derived - 120.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_energy_envelope() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{
            "bpm": 120.0,
            "energyEnvelope": {"values": [0.1, 0.5, 0.9, 1.0]}
        }"#;
        let result = parser.try_parse(json).unwrap();
        assert!(result.energy_envelope.is_some());
        assert_eq!(result.energy_envelope.unwrap().len(), 4);
    }

    #[test]
    fn test_event_skips_missing_time() {
        let parser = JsonPercussionAnalysisParser;
        let json = r#"{
            "bpm": 120.0,
            "events": [
                {"type": "kick"},
                {"type": "snare", "timeSeconds": 0.5}
            ]
        }"#;
        let result = parser.try_parse(json).unwrap();
        // Only the snare has a valid time; the kick is skipped
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].trigger_type, PercussionTriggerType::Snare);
    }
}
