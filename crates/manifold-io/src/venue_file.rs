//! Standalone venue files — export/import a project's [`StageLayout`] to a
//! `.manifoldvenue` JSON document, independent of the composition.
//!
//! The show and the venue are different lifetimes (`docs/MULTI_DISPLAY_DESIGN.md`
//! §5, D13): the composition is per-show, but stage layout + assignments +
//! advanced-flap calibration (keystone, trim, density cap — all carried on
//! `DisplayPlacement`/`OutputAdvanced` already) are per-venue. `StageLayout`
//! stays the single source of truth serialized inside `ProjectSettings` at
//! runtime; this module lets it travel separately — "load `corner-hotel.venue`,
//! play the same set."
//!
//! Same shape as `preset_file.rs` (export/import a self-contained JSON
//! document, no wrapper metadata): `StageLayout` alone is the complete venue
//! payload, since identity and advanced calibration already live on each
//! placement.

use std::path::Path;

use manifold_core::stage::StageLayout;

/// Canonical extension for a standalone venue file.
pub const VENUE_FILE_EXTENSION: &str = "manifoldvenue";

/// Failure modes for reading/writing a standalone venue file.
#[derive(Debug)]
pub enum VenueFileError {
    /// The file could not be read or written.
    Io(std::io::Error),
    /// The bytes on disk were not a valid `StageLayout` JSON document.
    Parse(serde_json::Error),
    /// Serializing the layout to JSON failed (should not happen for a valid layout).
    Serialize(serde_json::Error),
}

impl std::fmt::Display for VenueFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "venue file I/O error: {e}"),
            Self::Parse(e) => write!(f, "venue file is not a valid stage layout: {e}"),
            Self::Serialize(e) => write!(f, "failed to serialize venue: {e}"),
        }
    }
}

impl std::error::Error for VenueFileError {}

/// Serialize a stage layout to pretty JSON (human-readable, diff-friendly).
pub fn serialize_venue(layout: &StageLayout) -> Result<String, VenueFileError> {
    serde_json::to_string_pretty(layout).map_err(VenueFileError::Serialize)
}

/// Parse a stage layout from a JSON string.
pub fn deserialize_venue(json: &str) -> Result<StageLayout, VenueFileError> {
    serde_json::from_str(json).map_err(VenueFileError::Parse)
}

/// Write a stage layout to `path` as a standalone JSON document.
pub fn export_venue(layout: &StageLayout, path: &Path) -> Result<(), VenueFileError> {
    let json = serialize_venue(layout)?;
    std::fs::write(path, json).map_err(VenueFileError::Io)
}

/// Read a standalone venue JSON document from `path`.
pub fn import_venue(path: &Path) -> Result<StageLayout, VenueFileError> {
    let json = std::fs::read_to_string(path).map_err(VenueFileError::Io)?;
    deserialize_venue(&json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::stage::{DisplayIdentity, DisplayPlacement, OutputAdvanced, OutputId, Rotation};

    fn sample_layout() -> StageLayout {
        StageLayout {
            placements: vec![
                DisplayPlacement {
                    id: OutputId(0),
                    name: "Totem L".into(),
                    physical_size_mm: [540.0, 960.0],
                    native_resolution: [1080, 1920],
                    position_mm: [0.0, 0.0],
                    rotation: Rotation::R90,
                    identity: Some(DisplayIdentity {
                        uuid: Some("UUID-L".into()),
                        name: "LG Totem".into(),
                    }),
                    enabled: true,
                    advanced: OutputAdvanced::default(),
                },
                DisplayPlacement {
                    id: OutputId(1),
                    name: "Totem R".into(),
                    physical_size_mm: [540.0, 960.0],
                    native_resolution: [1080, 1920],
                    position_mm: [3500.0, 0.0],
                    rotation: Rotation::R90,
                    identity: None,
                    enabled: true,
                    advanced: OutputAdvanced::default(),
                },
            ],
        }
    }

    #[test]
    fn export_then_import_round_trips_byte_for_byte() {
        let layout = sample_layout();
        let json = serialize_venue(&layout).expect("serialize");
        let back = deserialize_venue(&json).expect("deserialize");
        assert_eq!(back, layout);

        // Re-serializing the imported layout reproduces the same JSON.
        let json2 = serialize_venue(&back).expect("re-serialize");
        assert_eq!(json, json2, "export/import is a stable round-trip");
    }

    #[test]
    fn export_to_disk_then_import_reads_it_back() {
        let layout = sample_layout();
        let path = std::env::temp_dir()
            .join(format!("manifold-venue-{}.{VENUE_FILE_EXTENSION}", std::process::id()));
        export_venue(&layout, &path).expect("export to disk");
        let imported = import_venue(&path).expect("import from disk");
        assert_eq!(imported, layout);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn import_rejects_malformed_json() {
        assert!(matches!(
            deserialize_venue("{ not valid json"),
            Err(VenueFileError::Parse(_)),
        ));
    }

    #[test]
    fn empty_layout_round_trips() {
        let layout = StageLayout::default();
        let json = serialize_venue(&layout).expect("serialize");
        let back = deserialize_venue(&json).expect("deserialize");
        assert!(back.is_empty());
    }
}
