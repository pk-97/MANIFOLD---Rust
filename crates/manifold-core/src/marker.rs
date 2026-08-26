use crate::id::MarkerId;
use crate::math::short_id;
use crate::types::MarkerColor;
use crate::units::Beats;
use serde::{Deserialize, Serialize};

/// A user-placed timeline marker at a specific beat position. In section
/// export every marker inside the export range is a cut point
/// (docs/SECTION_EXPORT_DESIGN.md D2) — there is no per-marker flavor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineMarker {
    pub id: MarkerId,
    pub beat: Beats,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: MarkerColor,
}

impl TimelineMarker {
    pub fn new(beat: Beats) -> Self {
        Self {
            id: MarkerId::new(short_id()),
            beat,
            name: String::new(),
            color: MarkerColor::default(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_color(mut self, color: MarkerColor) -> Self {
        self.color = color;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrips_name_and_beat() {
        let m = TimelineMarker::new(Beats::from_f32(8.0)).with_name("Drop");
        let json = serde_json::to_string(&m).expect("serialize marker");
        let back: TimelineMarker = serde_json::from_str(&json).expect("reload marker");
        assert_eq!(back.name, "Drop");
        assert_eq!(back.beat, Beats::from_f32(8.0));
    }

    /// Projects saved during the one-day `is_section_boundary` flavor
    /// (2026-08-26) carry an `isSectionBoundary` key. The field is gone —
    /// serde must ignore the unknown key, not fail the load.
    #[test]
    fn marker_with_retired_flavor_key_still_loads() {
        let m: TimelineMarker = serde_json::from_str(
            r#"{"id":"m1","beat":4.0,"name":"Verse","color":6,"isSectionBoundary":true}"#,
        )
        .expect("marker with retired flavor key must deserialize");
        assert_eq!(m.name, "Verse");
    }
}
