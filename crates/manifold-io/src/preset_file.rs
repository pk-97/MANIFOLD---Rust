//! Standalone preset files — export/import a single preset to a `.manifoldpreset`
//! JSON document.
//!
//! A preset is a self-contained [`EffectGraphDef`] (graph + `presetMetadata`
//! carrying params, ranges, curves, and bindings) — the exact same schema the
//! bundled effect/generator presets ship in. Exporting a project-embedded
//! ("forked") preset writes that def to a file someone else can drag-and-drop;
//! importing reads it back into an [`EffectGraphDef`] the caller installs as a
//! project-embedded preset. There is no per-instance state in the file — calibration
//! lives in the preset, which is the whole point of the fork model.

use std::path::Path;

use manifold_core::effect_graph_def::EffectGraphDef;

/// Canonical extension for a standalone preset file.
pub const PRESET_FILE_EXTENSION: &str = "manifoldpreset";

/// Failure modes for reading/writing a standalone preset file.
#[derive(Debug)]
pub enum PresetFileError {
    /// The file could not be read or written.
    Io(std::io::Error),
    /// The bytes on disk were not a valid [`EffectGraphDef`] JSON document.
    Parse(serde_json::Error),
    /// Serializing the def to JSON failed (should not happen for a valid def).
    Serialize(serde_json::Error),
}

impl std::fmt::Display for PresetFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "preset file I/O error: {e}"),
            Self::Parse(e) => write!(f, "preset file is not a valid preset graph: {e}"),
            Self::Serialize(e) => write!(f, "failed to serialize preset: {e}"),
        }
    }
}

impl std::error::Error for PresetFileError {}

/// Serialize a preset graph to pretty JSON (so a shared file is human-readable
/// and diff-friendly, matching the on-disk bundled presets).
pub fn serialize_preset(def: &EffectGraphDef) -> Result<String, PresetFileError> {
    serde_json::to_string_pretty(def).map_err(PresetFileError::Serialize)
}

/// Parse a preset graph from a JSON string.
pub fn deserialize_preset(json: &str) -> Result<EffectGraphDef, PresetFileError> {
    serde_json::from_str(json).map_err(PresetFileError::Parse)
}

/// Write a preset graph to `path` as a standalone JSON document.
pub fn export_preset(def: &EffectGraphDef, path: &Path) -> Result<(), PresetFileError> {
    let json = serialize_preset(def)?;
    std::fs::write(path, json).map_err(PresetFileError::Io)
}

/// Read a standalone preset JSON document from `path`.
pub fn import_preset(path: &Path) -> Result<EffectGraphDef, PresetFileError> {
    let json = std::fs::read_to_string(path).map_err(PresetFileError::Io)?;
    deserialize_preset(&json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
    };
    use manifold_core::preset_def::PresetKind;
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};

    /// A minimal forked preset: a single recalibrated card param carried on the
    /// def's `presetMetadata` (range widened, plus a binding scale) — the shape
    /// the fork model produces and a shared file must round-trip exactly.
    fn sample_def() -> EffectGraphDef {
        EffectGraphDef {
            version: 2,
            name: Some("My Oily Fluid".to_string()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("project.OilyFluid.variant1"),
                display_name: "My Oily Fluid".to_string(),
                category: "Generator".to_string(),
                osc_prefix: "oilyFluid".to_string(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: "speed".to_string(),
                    name: "Speed".to_string(),
                    min: 0.0,
                    max: 10.0,
                    default_value: 1.0,
                    whole_numbers: false,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: Vec::new(),
                    format_string: None,
                    osc_suffix: "speed".to_string(),
                    curve: Default::default(),
                    invert: false,
                    is_angle: false,
                    is_trigger_gate: false,
                    wraps: false,
                    section: None,
                    card_visible: true,
                }],
                bindings: vec![BindingDef {
                    id: "speed".to_string(),
                    label: "Speed".to_string(),
                    default_value: 1.0,
                    target: BindingTarget::Node {
                        node_id: manifold_core::NodeId::new("sim"),
                        param: "speed".to_string(),
                    },
                    convert: Default::default(),
                    user_added: false,
                    scale: 2.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn export_then_import_round_trips_byte_for_byte() {
        let def = sample_def();
        let json = serialize_preset(&def).expect("serialize");
        let back = deserialize_preset(&json).expect("deserialize");
        // The whole point of the file: calibration survives a share.
        let meta = back.preset_metadata.as_ref().expect("metadata survives");
        assert_eq!(meta.params[0].max, 10.0, "widened range survives export/import");
        assert_eq!(meta.bindings[0].scale, 2.0, "binding scale survives");
        assert_eq!(back.name.as_deref(), Some("My Oily Fluid"));
        // Re-serializing the imported def reproduces the same JSON.
        let json2 = serialize_preset(&back).expect("re-serialize");
        assert_eq!(json, json2, "export/import is a stable round-trip");
    }

    #[test]
    fn export_to_disk_then_import_reinstalls_into_a_project() {
        let def = sample_def();
        let path = std::env::temp_dir()
            .join(format!("manifold-preset-{}.{PRESET_FILE_EXTENSION}", std::process::id()));
        export_preset(&def, &path).expect("export to disk");
        let imported = import_preset(&path).expect("import from disk");

        // Install the imported preset into a fresh project as an embedded fork.
        let mut project = Project::default();
        let id = imported
            .preset_metadata
            .as_ref()
            .map(|m| m.id.clone())
            .expect("imported preset carries an id");
        project.upsert_embedded_preset(EmbeddedPreset {
            kind: PresetKind::Generator,
            def: imported,
            origin: EmbeddedOrigin::Saved,
        });
        assert!(
            project.embedded_preset(&id).is_some(),
            "imported preset installs as a project-embedded fork",
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn import_rejects_malformed_json() {
        assert!(matches!(
            deserialize_preset("{ not valid json"),
            Err(PresetFileError::Parse(_)),
        ));
    }
}
