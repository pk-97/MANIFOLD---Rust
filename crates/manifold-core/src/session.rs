//! Session mode data model — authored, persistent launch slots for the
//! Ableton-style scene/clip grid. See `docs/SESSION_MODE_DESIGN.md`.
//!
//! This module is P1 of that design: the model, `Project.session`, and serde
//! only. Runtime playback state (`SessionRuntime`) is a separate,
//! never-serialized type owned by `manifold-playback` (P2) — it is NOT part
//! of this module. Launch/grid-edit commands (`ContentCommand` variants,
//! `session_commands.rs`) are P2/P3.
//!
//! Content-agnostic by design (§11 of the doc): nothing here branches on
//! clip kind (video / image / generator / audio). `ClipSequence` reuses
//! `TimelineClip` unchanged.

use crate::clip::TimelineClip;
use crate::id::{LayerId, SceneId};
use crate::units::Beats;
use ahash::AHashMap;
use serde::{Deserialize, Serialize};

/// A loopable container of timed clips, launched as a unit from the session
/// grid. `clips[].start_beat` is RELATIVE to the sequence start (beat 0 =
/// sequence start), non-overlapping, sorted — mirrors a `Layer`'s lane but
/// scoped to one slot. The degenerate case (one clip at beat 0) is a plain
/// launchable clip; no special type. See `docs/SESSION_MODE_DESIGN.md` §3.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipSequence {
    /// Loop length in beats. Always >= the end beat of the last clip.
    #[serde(default)]
    pub length_beats: Beats,
    /// Timed clips, `start_beat` relative to sequence start.
    #[serde(default)]
    pub clips: Vec<TimelineClip>,
}

/// One cell in the session grid: a layer's launchable content for one scene.
/// Slots are sparse — a (layer, scene) pair with no `SessionSlot` is an empty
/// cell, not a slot with empty content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSlot {
    /// Owning layer, by identity — layers reorder, this does not move.
    pub layer_id: LayerId,
    /// Owning scene (row), by identity — scenes reorder too.
    pub scene_id: SceneId,
    /// The launchable content.
    #[serde(default)]
    pub sequence: ClipSequence,
    /// Display name; defaults from the first clip when created.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
}

/// A row in the session grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: SceneId,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
}

/// The session grid: scenes (rows) x layers (columns, via
/// `SessionSlot::layer_id`). Sparse — at most one slot per (layer, scene).
///
/// Pure authored data — this type holds no playback state. Runtime state
/// (`SessionRuntime`: which slot is playing per layer, pending quantized
/// launches, session-override set) is owned by `PlaybackEngine` on the
/// content thread, never serialized and never undo-wrapped. See
/// `docs/SESSION_MODE_DESIGN.md` §4.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGrid {
    /// Row order.
    #[serde(default)]
    pub scenes: Vec<Scene>,
    /// Flat slot list; at most one per (layer_id, scene_id).
    #[serde(default)]
    pub slots: Vec<SessionSlot>,

    /// Runtime lookup cache: (layer_id, scene_id) -> index into `slots`.
    /// Rebuilt on mutation — same pattern as `Timeline::clip_lookup`.
    #[serde(skip)]
    slot_lookup: AHashMap<(LayerId, SceneId), usize>,
    #[serde(skip)]
    slot_lookup_dirty: bool,
}

impl SessionGrid {
    /// True when the grid has no scenes and no slots. Used as the
    /// `Project::session` `skip_serializing_if` gate so pre-session projects
    /// round-trip byte-identically (the field is simply absent from JSON).
    pub fn is_empty(&self) -> bool {
        self.scenes.is_empty() && self.slots.is_empty()
    }

    /// Rebuild the O(1) (layer_id, scene_id) -> slot index cache. Called from
    /// `Project::on_after_deserialize`, same as `Timeline::rebuild_clip_lookup`.
    pub fn rebuild_slot_lookup(&mut self) {
        self.slot_lookup.clear();
        for (i, slot) in self.slots.iter().enumerate() {
            self.slot_lookup
                .insert((slot.layer_id.clone(), slot.scene_id.clone()), i);
        }
        self.slot_lookup_dirty = false;
    }

    /// Mark the lookup cache stale. Grid-edit commands (P3) call this after
    /// mutating `slots`.
    pub fn mark_slot_lookup_dirty(&mut self) {
        self.slot_lookup_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_grid() -> SessionGrid {
        let scene_a = SceneId::new("scene-a");
        let scene_b = SceneId::new("scene-b");
        let layer_1 = LayerId::new("layer-1");

        let mut grid = SessionGrid {
            scenes: vec![
                Scene { id: scene_a.clone(), name: "Intro".to_string(), color: Some([255, 0, 0]) },
                Scene { id: scene_b.clone(), name: "Drop".to_string(), color: None },
            ],
            slots: vec![SessionSlot {
                layer_id: layer_1.clone(),
                scene_id: scene_a.clone(),
                sequence: ClipSequence { length_beats: Beats(4.0), clips: Vec::new() },
                name: "Intro loop".to_string(),
                color: None,
            }],
            slot_lookup: AHashMap::new(),
            slot_lookup_dirty: false,
        };
        grid.rebuild_slot_lookup();
        grid
    }

    #[test]
    fn default_grid_is_empty() {
        let grid = SessionGrid::default();
        assert!(grid.is_empty());
        assert!(grid.scenes.is_empty());
        assert!(grid.slots.is_empty());
    }

    #[test]
    fn non_empty_grid_reports_not_empty() {
        let grid = sample_grid();
        assert!(!grid.is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let grid = sample_grid();
        let json = serde_json::to_string(&grid).expect("serialize SessionGrid");

        // Runtime-only fields never appear on the wire.
        assert!(!json.contains("slotLookup"));
        assert!(!json.contains("slot_lookup"));

        let mut restored: SessionGrid =
            serde_json::from_str(&json).expect("deserialize SessionGrid");

        assert_eq!(restored.scenes.len(), 2);
        assert_eq!(restored.scenes[0].name, "Intro");
        assert_eq!(restored.scenes[0].color, Some([255, 0, 0]));
        assert_eq!(restored.scenes[1].color, None);

        assert_eq!(restored.slots.len(), 1);
        assert_eq!(restored.slots[0].layer_id, grid.slots[0].layer_id);
        assert_eq!(restored.slots[0].scene_id, grid.slots[0].scene_id);
        assert_eq!(restored.slots[0].sequence.length_beats, Beats(4.0));
        assert_eq!(restored.slots[0].name, "Intro loop");

        // Lookup cache starts empty on fresh deserialize (skipped field) and
        // must be explicitly rebuilt, same contract as `Timeline::clip_lookup`.
        assert!(restored.slot_lookup.is_empty());
        restored.rebuild_slot_lookup();
        assert_eq!(
            restored.slot_lookup.get(&(grid.slots[0].layer_id.clone(), grid.slots[0].scene_id.clone())),
            Some(&0)
        );
    }

    #[test]
    fn empty_grid_serializes_to_minimal_json() {
        // Nothing but the two empty arrays should appear; no runtime fields.
        let grid = SessionGrid::default();
        let value: serde_json::Value =
            serde_json::to_value(&grid).expect("serialize default SessionGrid");
        let obj = value.as_object().expect("object");
        assert_eq!(obj.len(), 2, "expected exactly scenes + slots keys, got {obj:?}");
        assert!(obj.contains_key("scenes"));
        assert!(obj.contains_key("slots"));
    }

    #[test]
    fn clip_sequence_defaults() {
        let seq = ClipSequence::default();
        assert_eq!(seq.length_beats, Beats::ZERO);
        assert!(seq.clips.is_empty());
    }
}
