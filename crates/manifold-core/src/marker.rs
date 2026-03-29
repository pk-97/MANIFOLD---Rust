use serde::{Deserialize, Serialize};
use crate::id::MarkerId;
use crate::types::MarkerColor;
use crate::math::short_id;
use crate::units::Beats;

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
