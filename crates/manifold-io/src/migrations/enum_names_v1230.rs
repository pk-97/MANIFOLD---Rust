//! Files before 1.23.0 may store these enums as numbers (and a few as legacy
//! spellings). This rung rewrites each to the variant name the current derive
//! reads, using the same tables the old number-or-name impls used, so the
//! typed load stays names-only. Names already present are left alone.
//!
//! Generic keys (`color`, `source`, `waveform`, ...) are only rewritten under
//! the parents that really hold the enum, so a node param or map entry that
//! happens to share a key is never touched.

use serde_json::Value;

/// How one enum's stored value maps to the variant name.
struct EnumTable {
    /// Number -> variant name, exactly as the old impl mapped it.
    numbers: &'static [(i64, &'static str)],
    /// Variant for any other number, as the old impl's `_ =>` arm did.
    default: &'static str,
    /// Legacy spellings the old impl read that the derive does not.
    legacy: &'static [(&'static str, &'static str)],
    /// The old impl ignored the value entirely (single-variant enum).
    any_string_is_default: bool,
}

const BEAT_DIVISION: EnumTable = EnumTable {
    numbers: &[
        (0, "ThirtySecond"),
        (1, "Sixteenth"),
        (2, "Eighth"),
        (3, "Quarter"),
        (4, "Half"),
        (5, "Whole"),
        (6, "TwoWhole"),
        (7, "FourWhole"),
        (8, "EightWhole"),
        (9, "SixteenWhole"),
        (10, "ThirtyTwoWhole"),
        (11, "EighthDotted"),
        (12, "QuarterDotted"),
        (13, "HalfDotted"),
        (14, "WholeDotted"),
        (15, "TwoWholeDotted"),
        (16, "EighthTriplet"),
        (17, "QuarterTriplet"),
        (18, "HalfTriplet"),
        (19, "WholeTriplet"),
    ],
    default: "Quarter",
    legacy: &[
        ("_1_32", "ThirtySecond"),
        ("_1_16", "Sixteenth"),
        ("_1_8", "Eighth"),
        ("_1_4", "Quarter"),
        ("_1_2", "Half"),
        ("_1_1", "Whole"),
        ("_2_1", "TwoWhole"),
        ("_4_1", "FourWhole"),
        ("_8_1", "EightWhole"),
        ("_16_1", "SixteenWhole"),
        ("_32_1", "ThirtyTwoWhole"),
        ("_1_8_dot", "EighthDotted"),
        ("_1_4_dot", "QuarterDotted"),
        ("_1_2_dot", "HalfDotted"),
        ("_1_1_dot", "WholeDotted"),
        ("_2_1_dot", "TwoWholeDotted"),
        ("_1_8T", "EighthTriplet"),
        ("_1_4T", "QuarterTriplet"),
        ("_1_2T", "HalfTriplet"),
        ("_1_1T", "WholeTriplet"),
    ],
    any_string_is_default: false,
};

const BLEND_MODE: EnumTable = EnumTable {
    numbers: &[
        (0, "Normal"),
        (1, "Additive"),
        (2, "Multiply"),
        (3, "Screen"),
        (4, "Overlay"),
        (5, "Stencil"),
        (6, "Opaque"),
        (7, "Difference"),
        (8, "Exclusion"),
        (9, "Subtract"),
        (10, "ColorDodge"),
        (11, "Lighten"),
        (12, "Darken"),
    ],
    default: "Normal",
    legacy: &[],
    any_string_is_default: false,
};

const CLIP_DURATION_MODE: EnumTable = EnumTable {
    numbers: &[],
    default: "NoteOff",
    legacy: &[],
    any_string_is_default: true,
};

const CLOCK_AUTHORITY: EnumTable = EnumTable {
    numbers: &[(0, "Internal"), (1, "Link"), (2, "MidiClock"), (3, "Osc")],
    default: "Internal",
    legacy: &[],
    any_string_is_default: false,
};

const DRIVER_WAVEFORM: EnumTable = EnumTable {
    numbers: &[
        (0, "Sine"),
        (1, "Triangle"),
        (2, "Sawtooth"),
        (3, "Square"),
        (4, "Random"),
    ],
    default: "Sine",
    legacy: &[],
    any_string_is_default: false,
};

const LAYER_TYPE: EnumTable = EnumTable {
    numbers: &[
        (0, "Video"),
        (1, "Generator"),
        (2, "Group"),
        (3, "Audio"),
        (4, "Dmx"),
        (5, "Trigger"),
    ],
    default: "Video",
    legacy: &[],
    any_string_is_default: false,
};

const MARKER_COLOR: EnumTable = EnumTable {
    numbers: &[
        (0, "Red"),
        (1, "Orange"),
        (2, "Yellow"),
        (3, "Green"),
        (4, "Cyan"),
        (5, "Blue"),
        (6, "Purple"),
        (7, "White"),
    ],
    default: "Red",
    legacy: &[],
    any_string_is_default: false,
};

const OSC_SYNC_MODE: EnumTable = EnumTable {
    numbers: &[(0, "M4L"), (1, "AbletonOsc")],
    default: "M4L",
    legacy: &[],
    any_string_is_default: false,
};

const PERCUSSION_TRIGGER_TYPE: EnumTable = EnumTable {
    numbers: &[
        (0, "Unknown"),
        (1, "Kick"),
        (2, "Snare"),
        (3, "Clap"),
        (4, "Hat"),
        (5, "Perc"),
        (6, "Bass"),
        (7, "Synth"),
        (8, "Pad"),
        (9, "Vocal"),
        (10, "BassSustained"),
    ],
    default: "Unknown",
    legacy: &[],
    any_string_is_default: false,
};

const QUANTIZE_MODE: EnumTable = EnumTable {
    numbers: &[(0, "Off"), (1, "QuarterBeat"), (2, "Beat"), (3, "Bar")],
    default: "Off",
    legacy: &[],
    any_string_is_default: false,
};

const RESOLUTION_PRESET: EnumTable = EnumTable {
    numbers: &[
        (0, "HD720p"),
        (1, "FHD1080p"),
        (2, "QHD1440p"),
        (3, "UHD4K"),
        (4, "Square1080"),
        (5, "Portrait720"),
        (6, "Portrait1080"),
        (7, "Portrait1440"),
        (8, "Portrait4K"),
        (9, "Square4K"),
    ],
    default: "FHD1080p",
    legacy: &[
        ("HD_720p", "HD720p"),
        ("FHD_1080p", "FHD1080p"),
        ("QHD_1440p", "QHD1440p"),
        ("UHD_4K", "UHD4K"),
        ("Square_1080", "Square1080"),
        ("Portrait_720", "Portrait720"),
        ("Portrait_1080", "Portrait1080"),
        ("Portrait_1440", "Portrait1440"),
        ("Portrait_4K", "Portrait4K"),
        ("Square_4K", "Square4K"),
    ],
    any_string_is_default: false,
};

const TEMPO_POINT_SOURCE: EnumTable = EnumTable {
    numbers: &[
        (0, "Unknown"),
        (1, "Manual"),
        (2, "Link"),
        (3, "MidiClock"),
        (4, "Recorded"),
        (5, "AbletonOsc"),
    ],
    default: "Unknown",
    legacy: &[],
    any_string_is_default: false,
};

/// The table for `key` when the object holding it sits under `path` (the
/// object keys from the root down to that object, arrays not counted).
fn table_for(key: &str, path: &[String]) -> Option<&'static EnumTable> {
    let parent = path.last().map(String::as_str);
    let grandparent = path
        .len()
        .checked_sub(2)
        .and_then(|i| path.get(i))
        .map(String::as_str);
    match (key, parent) {
        ("beatDivision", Some("drivers")) => Some(&BEAT_DIVISION),
        ("waveform", Some("drivers")) => Some(&DRIVER_WAVEFORM),
        ("defaultBlendMode", Some("layers")) => Some(&BLEND_MODE),
        ("layerType", Some("layers")) => Some(&LAYER_TYPE),
        ("durationMode", Some("layers" | "mappings")) => Some(&CLIP_DURATION_MODE),
        ("clockAuthority", Some("settings")) => Some(&CLOCK_AUTHORITY),
        ("oscSyncMode", Some("settings")) => Some(&OSC_SYNC_MODE),
        ("quantizeMode", Some("settings")) => Some(&QUANTIZE_MODE),
        ("resolutionPreset", Some("settings")) => Some(&RESOLUTION_PRESET),
        ("triggerType", Some("instruments" | "events")) => Some(&PERCUSSION_TRIGGER_TYPE),
        ("color", Some("markers")) => Some(&MARKER_COLOR),
        ("startTempoSource" | "endTempoSource", Some("recordedClips")) => Some(&TEMPO_POINT_SOURCE),
        ("recordedProjectBpmSource", Some("recordingProvenance")) => Some(&TEMPO_POINT_SOURCE),
        ("source", Some("points")) if grandparent == Some("tempoMap") => Some(&TEMPO_POINT_SOURCE),
        ("source", Some("recordedTempoLane" | "tempoChanges"))
            if grandparent == Some("recordingProvenance") =>
        {
            Some(&TEMPO_POINT_SOURCE)
        }
        _ => None,
    }
}

/// The variant name to write for `value`, or `None` to leave it as is.
fn rewrite(value: &Value, table: &EnumTable) -> Option<&'static str> {
    match value {
        Value::Number(n) => Some(
            n.as_i64()
                .and_then(|i| table.numbers.iter().find(|(k, _)| *k == i))
                .map_or(table.default, |(_, name)| name),
        ),
        Value::String(s) => {
            if table.any_string_is_default {
                return (s != table.default).then_some(table.default);
            }
            table
                .legacy
                .iter()
                .find(|(old, _)| old == s)
                .map(|(_, name)| *name)
        }
        _ => None,
    }
}

fn walk(value: &mut Value, path: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if matches!(child, Value::Number(_) | Value::String(_)) {
                    if let Some(name) = table_for(key, path).and_then(|table| rewrite(child, table))
                    {
                        *child = Value::String(name.to_string());
                    }
                } else {
                    path.push(key.clone());
                    walk(child, path);
                    path.pop();
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, path);
            }
        }
        _ => {}
    }
}

pub fn migrate(root: &mut Value) {
    walk(root, &mut Vec::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::percussion_analysis::PercussionTriggerType;
    use manifold_core::project::Project;
    use manifold_core::types::{
        BeatDivision, BlendMode, ClipDurationMode, ClockAuthority, DriverWaveform, LayerType,
        MarkerColor, OscSyncMode, QuantizeMode, ResolutionPreset, TempoPointSource,
    };
    use serde_json::json;

    /// A 1.22.0 project with every enum location. `n` picks numbers or names.
    fn project_json(numeric: bool) -> Value {
        let v = |num: i64, name: &str| if numeric { json!(num) } else { json!(name) };
        json!({
            "projectVersion": "1.22.0",
            "projectName": "enum test",
            "timeline": {
                "layers": [{
                    "name": "L",
                    "layerType": v(3, "Audio"),
                    "defaultBlendMode": v(7, "Difference"),
                    "durationMode": v(2, "NoteOff"),
                    "effects": [{
                        "effectType": "x",
                        "drivers": [{
                            "paramId": "p",
                            "beatDivision": v(4, "Half"),
                            "waveform": v(1, "Triangle")
                        }]
                    }],
                    "clips": [{
                        "id": "c0",
                        "audioDetection": {
                            "config": {
                                "quantizeOn": true,
                                "quantizeStepBeats": 0.25,
                                "onsetCompensation": 0.0,
                                "instruments": [{
                                    "triggerType": v(2, "Snare"),
                                    "enabled": true,
                                    "sensitivity": 0.5
                                }]
                            }
                        }
                    }]
                }],
                "markers": [{ "id": "m1", "beat": 4.0, "color": v(5, "Blue") }]
            },
            "midiConfig": { "mappings": [{ "midiNote": 60, "durationMode": v(2, "NoteOff") }] },
            "settings": {
                "clockAuthority": v(2, "MidiClock"),
                "oscSyncMode": v(1, "AbletonOsc"),
                "quantizeMode": v(2, "Beat"),
                "resolutionPreset": v(1, "FHD1080p"),
                "extra": { "waveform": 0.5, "color": 0.25 }
            },
            "tempoMap": { "points": [
                { "beat": 0.0, "bpm": 120.0, "source": v(3, "MidiClock") }
            ]},
            "recordingProvenance": {
                "recordedClips": [{
                    "clipId": "c1",
                    "startTempoSource": v(4, "Recorded"),
                    "endTempoSource": v(5, "AbletonOsc")
                }],
                "tempoChanges": [
                    { "timeSeconds": 1.0, "beat": 2.0, "bpm": 130.0, "source": v(2, "Link") }
                ],
                "recordedTempoLane": [
                    { "beat": 0.0, "bpm": 110.0, "source": v(1, "Manual") }
                ],
                "recordedProjectBpmSource": v(4, "Recorded")
            }
        })
    }

    fn load(json: &Value) -> Project {
        let migrated = crate::migrate::migrate_if_needed(&json.to_string()).unwrap();
        serde_json::from_str(&migrated).unwrap()
    }

    fn assert_variants(p: &Project) {
        let layer = &p.timeline.layers[0];
        assert_eq!(layer.layer_type, LayerType::Audio);
        assert_eq!(layer.default_blend_mode, BlendMode::Difference);
        assert_eq!(layer.duration_mode, Some(ClipDurationMode::NoteOff));
        let driver = &layer.effects.as_ref().unwrap()[0].drivers.as_ref().unwrap()[0];
        assert_eq!(driver.beat_division, BeatDivision::Half);
        assert_eq!(driver.waveform, DriverWaveform::Triangle);
        let det = layer.clips[0].audio_detection.as_ref().unwrap();
        assert_eq!(
            det.config.instruments[0].trigger_type,
            PercussionTriggerType::Snare
        );
        assert_eq!(p.timeline.markers[0].color, MarkerColor::Blue);
        assert_eq!(
            p.midi_config.mappings[0].duration_mode,
            Some(ClipDurationMode::NoteOff)
        );
        assert_eq!(p.settings.clock_authority, ClockAuthority::MidiClock);
        assert_eq!(p.settings.osc_sync_mode, OscSyncMode::AbletonOsc);
        assert_eq!(p.settings.quantize_mode, QuantizeMode::Beat);
        assert_eq!(p.settings.resolution_preset, ResolutionPreset::FHD1080p);
        assert_eq!(p.tempo_map.points()[0].source, TempoPointSource::MidiClock);
        let rec = &p.recording_provenance;
        assert_eq!(
            rec.recorded_clips[0].start_tempo_source,
            TempoPointSource::Recorded
        );
        assert_eq!(
            rec.recorded_clips[0].end_tempo_source,
            TempoPointSource::AbletonOsc
        );
        assert_eq!(rec.tempo_changes[0].source, TempoPointSource::Link);
        assert_eq!(rec.recorded_tempo_lane[0].source, TempoPointSource::Manual);
        assert_eq!(rec.recorded_project_bpm_source, TempoPointSource::Recorded);
    }

    #[test]
    fn numbers_become_the_variants_the_old_tables_named() {
        let p = load(&project_json(true));
        assert_variants(&p);
        assert_eq!(p.project_version, "1.24.0");
    }

    #[test]
    fn names_pass_through_unchanged() {
        let before = project_json(false);
        let mut after: Value =
            serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap())
                .unwrap();
        assert_eq!(after["projectVersion"], "1.24.0");
        let mut expected = before;
        expected["projectVersion"] = json!("1.24.0");
        after["projectVersion"] = json!("1.24.0");
        assert_eq!(after, expected);
        assert_variants(&load(&project_json(false)));
    }

    #[test]
    fn unknown_numbers_map_to_the_old_default() {
        let mut j = project_json(true);
        j["settings"]["resolutionPreset"] = json!(99);
        j["settings"]["clockAuthority"] = json!(-4);
        j["timeline"]["layers"][0]["layerType"] = json!(42);
        j["timeline"]["markers"][0]["color"] = json!(1.5);
        let p = load(&j);
        assert_eq!(p.settings.resolution_preset, ResolutionPreset::FHD1080p);
        assert_eq!(p.settings.clock_authority, ClockAuthority::Internal);
        assert_eq!(p.timeline.layers[0].layer_type, LayerType::Video);
        assert_eq!(p.timeline.markers[0].color, MarkerColor::Red);
    }

    #[test]
    fn legacy_spellings_become_names() {
        let mut j = project_json(false);
        j["settings"]["resolutionPreset"] = json!("QHD_1440p");
        j["timeline"]["layers"][0]["effects"][0]["drivers"][0]["beatDivision"] = json!("_1_4T");
        let p = load(&j);
        assert_eq!(p.settings.resolution_preset, ResolutionPreset::QHD1440p);
        let driver = &p.timeline.layers[0].effects.as_ref().unwrap()[0]
            .drivers
            .as_ref()
            .unwrap()[0];
        assert_eq!(driver.beat_division, BeatDivision::QuarterTriplet);
    }

    #[test]
    fn same_keys_outside_their_parents_are_untouched() {
        let mut j = project_json(true);
        j["settings"]["extra"] =
            json!({ "waveform": 1, "color": 3, "source": 2, "beatDivision": 4 });
        j["timeline"]["layers"][0]["params"] =
            json!({ "waveform": 3, "color": 2, "triggerType": 5 });
        let mut root = j.clone();
        migrate(&mut root);
        assert_eq!(root["settings"]["extra"], j["settings"]["extra"]);
        assert_eq!(
            root["timeline"]["layers"][0]["params"],
            j["timeline"]["layers"][0]["params"]
        );
    }

    #[test]
    fn saved_after_migration_has_names_and_current_version() {
        let p = load(&project_json(true));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/enum_names_v1230_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("roundtrip_{}.manifold", std::process::id()));
        crate::saver::save_project_v1(&p, &path).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let on_disk: Value = serde_json::from_str(&json).unwrap();
        let reloaded = load(&on_disk);
        assert_variants(&reloaded);
        assert_eq!(reloaded.project_version, "1.24.0");
        let v = on_disk;
        assert_eq!(v["projectVersion"], "1.24.0");
        assert_eq!(v["settings"]["clockAuthority"], "MidiClock");
        assert_eq!(v["timeline"]["layers"][0]["layerType"], "Audio");
    }
}
