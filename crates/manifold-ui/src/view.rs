//! UI-local view-models of engine domain entities.
//!
//! Phase 5 layering inversion: where the engine has a rich domain model
//! (`Layer`, `ParamSlot`, `TimelineMarker`), the UI consumes a narrow *view* of
//! it — only the fields it renders. `manifold-app` builds these from the engine
//! model when it pushes render data (`rebuild_mapper_layout`, `sync_values`,
//! `set_markers`, the `TimelineEditingHost::layers()` cache). See
//! `docs/UI_LAYERING_INVERSION.md`.

use crate::types::{LayerType, MarkerColor};
use manifold_foundation::{Beats, LayerId, MarkerId};
use std::collections::HashSet;

/// Region-based timeline selection state. UI-local mirror of
/// `manifold_core::selection::SelectionRegion` (the rich selection-state region,
/// distinct from the simpler `panels::viewport::SelectionRegion` render rect).
/// Owned by `UIState`; the app translates it to/from the engine.
#[derive(Debug, Clone, Default)]
pub struct SelectionRegion {
    pub start_beat: Beats,
    pub end_beat: Beats,
    pub is_active: bool,
    pub start_layer_id: Option<LayerId>,
    pub end_layer_id: Option<LayerId>,
    pub selected_layer_ids: HashSet<LayerId>,
}

impl SelectionRegion {
    pub fn contains_beat(&self, beat: Beats) -> bool {
        // Half-open [start, end) via the shared interval primitive.
        crate::hit::Span::new(self.start_beat.as_f32(), self.end_beat.as_f32())
            .contains(beat.as_f32())
    }

    /// Whether a layer is in this region (HashSet lookup by stable id).
    pub fn contains_layer_id(&self, id: &LayerId) -> bool {
        self.selected_layer_ids.contains(id)
    }

    pub fn duration_beats(&self) -> Beats {
        self.end_beat - self.start_beat
    }

    /// Clear the selection region.
    pub fn clear(&mut self) {
        self.start_beat = Beats::ZERO;
        self.end_beat = Beats::ZERO;
        self.is_active = false;
        self.start_layer_id = None;
        self.end_layer_id = None;
        self.selected_layer_ids.clear();
    }

    /// Resolve the start/end layer ids to a normalized `(min, max)` index range
    /// against the given layer list. `None` if neither end is set or found.
    pub fn layer_index_range(&self, layers: &[UiLayer]) -> Option<(usize, usize)> {
        let start_idx = self
            .start_layer_id
            .as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));
        let end_idx = self
            .end_layer_id
            .as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));
        match (start_idx, end_idx) {
            (Some(s), Some(e)) => Some((s.min(e), s.max(e))),
            (Some(s), None) => Some((s, s)),
            (None, Some(e)) => Some((e, e)),
            (None, None) => None,
        }
    }
}

/// The UI's view of one timeline layer — the field subset the Y-layout, the
/// layer headers, and region selection read. Built from `manifold_core::layer::Layer`.
#[derive(Debug, Clone, Default)]
pub struct UiLayer {
    pub layer_id: LayerId,
    pub parent_layer_id: Option<LayerId>,
    pub layer_type: LayerType,
    pub is_collapsed: bool,
}

impl UiLayer {
    /// Whether this layer is a group (children nest under it).
    pub fn is_group(&self) -> bool {
        self.layer_type == LayerType::Group
    }
}

/// The UI's view of one effect/generator parameter slot — the values the param
/// card pushes into its sliders. Built from `manifold_core::effects::ParamSlot`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UiParamSlot {
    /// Effective (post-modulation) value — what the slider displays.
    pub value: f32,
    /// User-intended base (pre-modulation) value.
    pub base: f32,
    pub exposed: bool,
}

impl UiParamSlot {
    /// An exposed slot with the given value (base seeded to the same value).
    #[inline]
    pub const fn exposed(value: f32) -> Self {
        Self {
            value,
            base: value,
            exposed: true,
        }
    }
}

/// The UI's view of one timeline marker. Built from
/// `manifold_core::marker::TimelineMarker`.
#[derive(Debug, Clone, Default)]
pub struct UiMarker {
    pub id: MarkerId,
    pub beat: Beats,
    pub name: String,
    pub color: MarkerColor,
}

impl UiMarker {
    /// A marker at `beat` with default color and no name. Mirrors
    /// `TimelineMarker::new` but mints no id (callers/tests set it).
    pub fn new(beat: Beats) -> Self {
        Self {
            id: MarkerId::default(),
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
