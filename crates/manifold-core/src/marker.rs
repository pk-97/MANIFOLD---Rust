use crate::id::MarkerId;
use crate::math::short_id;
use crate::types::MarkerColor;
use crate::units::Beats;
use serde::{Deserialize, Serialize};

/// A user-placed timeline marker at a specific beat position.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineMarker {
    pub id: MarkerId,
    pub beat: Beats,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: MarkerColor,
    /// Whether this marker is a section boundary ("cut here") for section
    /// export. Defaults false so pre-flag projects and markers never slice an
    /// export on load. See docs/SECTION_EXPORT_DESIGN.md D3.
    #[serde(default)]
    pub is_section_boundary: bool,
}

impl TimelineMarker {
    pub fn new(beat: Beats) -> Self {
        Self {
            id: MarkerId::new(short_id()),
            beat,
            name: String::new(),
            color: MarkerColor::default(),
            is_section_boundary: false,
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

    /// Mark this marker as a section boundary (builder, shaped like `with_name`).
    pub fn as_section(mut self) -> Self {
        self.is_section_boundary = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::Timeline;

    /// SECTION_EXPORT_DESIGN.md section 4 (Invariants & enforcement): a marker
    /// serialized before the flag existed loads with `is_section_boundary ==
    /// false` — old projects never slice an export on load. The `#[serde(default)]`
    /// is the contract; this pins the JSON shape (camelCase `isSectionBoundary`).
    #[test]
    fn marker_missing_flag_defaults_false() {
        // Pre-flag marker JSON: no `isSectionBoundary` key at all.
        let m: TimelineMarker = serde_json::from_str(
            r#"{"id":"m1","beat":4.0,"name":"Verse","color":6}"#,
        )
        .expect("pre-flag marker JSON must deserialize");
        assert!(!m.is_section_boundary, "missing flag must default to false");
        assert_eq!(m.name, "Verse");
    }

    #[test]
    fn marker_flag_roundtrips_camelcase() {
        let m = TimelineMarker::new(Beats::from_f32(8.0))
            .with_name("Drop")
            .as_section();
        let json = serde_json::to_string(&m).expect("serialize marker");
        assert!(
            json.contains("\"isSectionBoundary\":true"),
            "flag must serialize as camelCase `isSectionBoundary`: {json}"
        );

        let back: TimelineMarker =
            serde_json::from_str(&json).expect("reload marker");
        assert!(back.is_section_boundary, "flag true must survive round-trip");
    }

    /// The full-project variant of the invariant: a timeline carrying markers
    /// but no flag on any of them loads every marker with the flag false.
    #[test]
    fn timeline_markers_missing_flag_default_false() {
        let t: Timeline = serde_json::from_str(
            r#"{
                "layers": [],
                "markers": [
                    {"id":"a","beat":1.0,"name":"Intro","color":0},
                    {"id":"b","beat":5.0,"name":"Break","color":2}
                ]
            }"#,
        )
        .expect("timeline with pre-flag markers must deserialize");
        assert_eq!(t.markers.len(), 2);
        for m in &t.markers {
            assert!(!m.is_section_boundary, "pre-flag markers must load false");
        }
    }
}
