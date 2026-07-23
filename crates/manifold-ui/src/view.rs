//! UI-local view-models of engine domain entities.
//!
//! Phase 5 layering inversion: where the engine has a rich domain model
//! (`Layer`, `ParamSlot`, `TimelineMarker`), the UI consumes a narrow *view* of
//! it — only the fields it renders. `manifold-app` builds these from the engine
//! model when it pushes render data (`rebuild_mapper_layout`, `sync_values`,
//! `set_markers`, the `TimelineEditingHost::layers()` cache). See
//! `docs/UI_LAYERING_INVERSION.md`.

use crate::types::{LayerType, MarkerColor};
use manifold_foundation::{Beats, EffectId, LayerId, MarkerId, ParamId};
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
    /// A fresh active region over `[start_beat, end_beat)` with no layer span
    /// yet (the caller fills `selected_layer_ids` / `start_layer_id` /
    /// `end_layer_id`). Since D1 a region only ever exists inside
    /// `TimelineSelection::TimeRange`, so it is always active — `is_active`
    /// remains as the signal `selection_region_to_core` copies to the engine
    /// (EditingService's duplicate/delete region-mode still reads it).
    pub fn active(start_beat: Beats, end_beat: Beats) -> Self {
        Self {
            start_beat,
            end_beat,
            is_active: true,
            ..Self::default()
        }
    }

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
    /// Number of visible automation lane strips this layer's Y-layout must
    /// reserve room for (0 when automation mode is off, the layer is
    /// collapsed, or it carries no enabled lanes). `CoordinateMapper::
    /// layer_height` is the single place this count turns into pixels — see
    /// `docs/AUTOMATION_LANES_DESIGN.md` §7. Computed by
    /// `ui_translate::layers_to_ui_for_layout`, never by the plain
    /// `layers_to_ui` (which defaults this to 0 for callers that only need
    /// selection-shape fields, not the Y-layout).
    pub automation_lane_count: usize,
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

/// One entry of the per-frame value channel: a param's borrowed manifest id
/// paired with its slot. The per-frame value syncs iterate these and JOIN each
/// slot onto the built row that carries the same id (BUG-313) — never by
/// position. The `&str` borrows the manifest for the duration of the sync; it
/// is never cloned (the manifest ids are `Cow::Owned` for scene-stamped
/// params, so a clone per param per frame per window is a hot-path violation).
pub type ParamSlotEntry<'a> = (&'a str, UiParamSlot);

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

// ── Automation lanes (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) ────────────
//
// UI-local mirror of `manifold_core::effects::{AutomationLane, AutomationPoint,
// SegmentShape}`. Built once per structural sync by `ui_translate::
// layer_automation_lanes_to_ui` and consumed by the timeline viewport's
// lane-strip renderer. `EffectId`/`ParamId` are shared `manifold-foundation`
// types (no translation needed — see `ui_translate.rs`'s header comment);
// only the core-only point/shape types get a UI-local copy.

/// UI-local mirror of `manifold_core::effects::SegmentShape`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiSegmentShape {
    Linear,
    Hold,
    Curved(f32),
}

/// UI-local mirror of `manifold_core::effects::AutomationPoint`. `value_norm`
/// is the param-range value already normalized to `0..1` by the translator
/// (which alone has the registry access to resolve a param's min/max) — an
/// affine normalization commutes with every segment shape's interpolation, so
/// sampling normalized points here gives the identical curve the core sampler
/// would produce on raw values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiAutomationPoint {
    pub beat: Beats,
    pub value_norm: f32,
    pub shape: UiSegmentShape,
}

/// UI-local mirror of `manifold_core::GraphTarget` — kept local because
/// `manifold-ui` depends only on `manifold-foundation`, not `manifold-core`
/// (see this file's header comment / `docs/UI_LAYERING_INVERSION.md`), so the
/// real `GraphTarget` isn't reachable here. Carries the addressing an
/// automation edit needs to build `AddAutomationPointCommand` /
/// `MoveAutomationPointCommand` / `RemoveAutomationPointCommand` on the
/// `manifold-app` side (`editing_host.rs` converts this 1:1 back to
/// `manifold_core::GraphTarget` — both variants wrap the identical
/// `EffectId`/`LayerId` types re-exported from `manifold-foundation`, so the
/// conversion is a plain clone, never a lookup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiGraphTarget {
    Effect(EffectId),
    Generator(LayerId),
}

/// One lane strip's read-only render data: which param, whether it draws
/// grayed (overridden), and the breakpoints the viewport samples into a
/// screen-space polyline. `effect_id` + `param_id` are the identity key that
/// matches `ContentState::automation_latched_params` for override graying —
/// this is the PresetInstance's own (possibly synthetic, for a generator)
/// `EffectId`, distinct from `target`, which is the ADDRESSING key
/// (`GraphTarget::Generator(layer_id)` for a generator's own params, since
/// `PresetInstance::id` is documented synthetic there — see
/// `manifold_core::effects::PresetInstance::id`'s doc comment). Both are
/// needed: `effect_id` for the latch-match, `target` for building edit
/// commands.
#[derive(Debug, Clone)]
pub struct UiAutomationLane {
    pub effect_id: EffectId,
    pub param_id: ParamId,
    pub target: UiGraphTarget,
    /// Display label for the strip's chooser slot (e.g. "Mirror: amount").
    pub label: String,
    /// Sorted ascending by `beat` (mirrors `AutomationLane::points`'
    /// write-time invariant — the translator copies the core order as-is).
    pub points: Vec<UiAutomationPoint>,
    /// The param's resolved range (`ResolvedParam::min/max` — override range
    /// when the chevron popover recalibrated it, else the catalog range).
    /// THE FOOTGUN: `points[i].value_norm` is already `0..1` (divided by this
    /// range by `ui_translate::push_instance_automation_lanes`), but
    /// `AutomationPoint.value` in the edit commands is PARAM RANGE. Editing
    /// code must denormalize screen-space Y back through this range —
    /// [`Self::denormalize`] is the one place that happens.
    pub param_min: f32,
    pub param_max: f32,
    /// Whether the param is integral (registry `whole_numbers`/`value_labels`,
    /// or an integral user-binding conversion) — `docs/AUTOMATION_LANES_DESIGN.md`
    /// §8: "Enum/int-backed params: author with Hold segments." Newly-clicked
    /// points on such a param default to `Hold` instead of `Linear` — see
    /// [`Self::new_point_shape`].
    pub whole_numbers: bool,
    /// True for a chosen-but-not-yet-automated param (P5, §7 addendum's
    /// "first-draw path") — a single synthetic point at the param's current
    /// base value, no backing `AutomationLane` on the core side yet.
    /// `viewport.rs`'s `automation_lane_screens` skips dot emission for
    /// these so the flat line shows with no breakpoint until the user's
    /// first click actually creates one. Real (core-backed) lanes are always
    /// `false`.
    pub placeholder: bool,
}

impl UiAutomationLane {
    /// Inverse of `ui_translate::push_instance_automation_lanes`'s
    /// `(value - min) / range` normalization — screen-space Y (already
    /// converted to `0..1` by the caller) back to the param-range value
    /// `AutomationPoint.value` expects. Clamps to `[min, max]` since a
    /// dragged Y can go slightly out of the strip's rect.
    pub fn denormalize(&self, value_norm: f32) -> f32 {
        let n = value_norm.clamp(0.0, 1.0);
        self.param_min + n * (self.param_max - self.param_min)
    }

    /// Default `SegmentShape` for a NEWLY clicked/added point (§8: enum/int
    /// params author with `Hold`; everything else defaults `Linear`, matching
    /// the shape leaving-this-point convention `AutomationPoint::shape` docs).
    pub fn new_point_shape(&self) -> UiSegmentShape {
        if self.whole_numbers {
            UiSegmentShape::Hold
        } else {
            UiSegmentShape::Linear
        }
    }
}

/// Identifies one breakpoint for selection / deletion — the (target, param,
/// beat) triple is stable identity for a point that hasn't been dragged this
/// frame (mirrors the core commands' own by-beat point identity, see
/// `manifold-editing/src/commands/automation.rs`'s module doc).
#[derive(Debug, Clone, PartialEq)]
pub struct UiAutomationPointRef {
    pub target: UiGraphTarget,
    pub param_id: ParamId,
    pub beat: Beats,
}

impl UiAutomationLane {
    /// Mirror of `AutomationLane::value_at`, operating on the pre-normalized
    /// `0..1` values so the viewport never reaches back into manifold-core to
    /// draw a line. Same before-first / after-last / per-segment-shape rules.
    pub fn value_at_norm(&self, beat: Beats) -> f32 {
        match self.points.as_slice() {
            [] => 0.0,
            [only] => only.value_norm,
            points => {
                let first = &points[0];
                if beat.0 <= first.beat.0 {
                    return first.value_norm;
                }
                let last = &points[points.len() - 1];
                if beat.0 >= last.beat.0 {
                    return last.value_norm;
                }
                let idx = match points.binary_search_by(|p| {
                    p.beat.0.partial_cmp(&beat.0).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Ok(i) => i,
                    Err(i) => i - 1,
                };
                let a = &points[idx];
                let b = &points[idx + 1];
                let span = (b.beat.0 - a.beat.0) as f32;
                if span <= 0.0 {
                    return a.value_norm;
                }
                let t = ((beat.0 - a.beat.0) as f32 / span).clamp(0.0, 1.0);
                match a.shape {
                    UiSegmentShape::Hold => a.value_norm,
                    UiSegmentShape::Linear => a.value_norm + (b.value_norm - a.value_norm) * t,
                    UiSegmentShape::Curved(bend) => {
                        let shaped = automation_segment_bend(t, bend);
                        a.value_norm + (b.value_norm - a.value_norm) * shaped
                    }
                }
            }
        }
    }
}

/// Mirror of `manifold_core::effects::segment_bend` — the power-curve bend
/// for a `Curved` segment's interpolation parameter `t` (already `[0, 1]`).
/// Kept byte-identical to the core formula so a lane drawn here matches what
/// actually samples on the content thread. `pub(crate)`: also used by
/// `automation_hit_tester.rs` to compute the exact on-screen segment Y a
/// segment-drag/curve-bend hit-test must agree with (P4 Unit B,
/// `docs/AUTOMATION_LANES_DESIGN.md` §7).
pub(crate) fn automation_segment_bend(t: f32, bend: f32) -> f32 {
    let bend = bend.clamp(-1.0, 1.0);
    if bend == 0.0 {
        return t;
    }
    let exponent = if bend > 0.0 {
        1.0 + bend * 3.0
    } else {
        1.0 / (1.0 - bend * 3.0)
    };
    t.powf(exponent)
}

#[cfg(test)]
mod automation_lane_tests {
    use super::*;

    fn lane(points: Vec<UiAutomationPoint>) -> UiAutomationLane {
        UiAutomationLane {
            effect_id: EffectId::new("fx"),
            param_id: ParamId::from("amount"),
            target: UiGraphTarget::Effect(EffectId::new("fx")),
            label: "Fx: amount".into(),
            points,
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
            placeholder: false,
        }
    }

    fn pt(beat: f64, value_norm: f32, shape: UiSegmentShape) -> UiAutomationPoint {
        UiAutomationPoint { beat: Beats(beat), value_norm, shape }
    }

    #[test]
    fn empty_lane_samples_zero() {
        assert_eq!(lane(vec![]).value_at_norm(Beats(1.0)), 0.0);
    }

    #[test]
    fn before_first_and_after_last_clamp() {
        let l = lane(vec![
            pt(4.0, 0.2, UiSegmentShape::Linear),
            pt(8.0, 0.8, UiSegmentShape::Linear),
        ]);
        assert_eq!(l.value_at_norm(Beats(0.0)), 0.2);
        assert_eq!(l.value_at_norm(Beats(100.0)), 0.8);
    }

    #[test]
    fn linear_segment_interpolates_at_midpoint() {
        let l = lane(vec![
            pt(0.0, 0.0, UiSegmentShape::Linear),
            pt(4.0, 1.0, UiSegmentShape::Linear),
        ]);
        assert!((l.value_at_norm(Beats(2.0)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn hold_segment_steps() {
        let l = lane(vec![
            pt(0.0, 0.2, UiSegmentShape::Hold),
            pt(4.0, 0.9, UiSegmentShape::Linear),
        ]);
        assert_eq!(l.value_at_norm(Beats(3.9)), 0.2);
    }

    #[test]
    fn curved_segment_matches_core_bend_formula_at_t_half() {
        // bend = 1.0 -> exponent 4.0 -> shaped(0.5) = 0.5^4 = 0.0625
        let l = lane(vec![
            pt(0.0, 0.0, UiSegmentShape::Curved(1.0)),
            pt(4.0, 1.0, UiSegmentShape::Linear),
        ]);
        let got = l.value_at_norm(Beats(2.0));
        assert!((got - 0.0625).abs() < 1e-5, "got {got}");
    }

    #[test]
    fn denormalize_round_trips_through_a_non_unit_range() {
        // The footgun this pins: a lane whose param range isn't 0..1 (e.g. a
        // 20..20000 Hz cutoff) must denormalize a clicked/dragged 0..1 screen
        // position back to the exact param-range value ui_translate would have
        // normalized FROM — add→render must land where the user clicked.
        let mut l = lane(vec![]);
        l.param_min = 20.0;
        l.param_max = 20020.0;
        assert_eq!(l.denormalize(0.0), 20.0);
        assert_eq!(l.denormalize(1.0), 20020.0);
        assert_eq!(l.denormalize(0.5), 10020.0);
        // Out-of-range Y (drag past the strip edge) clamps rather than
        // extrapolating past the param's own min/max.
        assert_eq!(l.denormalize(-0.2), 20.0);
        assert_eq!(l.denormalize(1.2), 20020.0);
    }

    #[test]
    fn new_point_shape_defaults_hold_for_whole_numbers_params() {
        let mut l = lane(vec![]);
        assert_eq!(l.new_point_shape(), UiSegmentShape::Linear);
        l.whole_numbers = true;
        assert_eq!(l.new_point_shape(), UiSegmentShape::Hold);
    }
}
