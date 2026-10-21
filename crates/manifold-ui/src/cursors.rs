//! Timeline cursor shapes, mapped to the platform's built-in cursor icons.

/// The cursor shapes used by the timeline UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCursor {
    Crosshair,
    /// Standard arrow.
    Default,
    /// ↔ horizontal resize.
    /// Used for: inspector width drag, trim handle hover
    ResizeHorizontal,
    /// ↕ vertical resize.
    /// Used for: video/timeline split handle
    ResizeVertical,
    /// ✥ four-way move.
    /// Used for: clip body drag
    Move,
    /// ⊘ not-allowed.
    /// Used for: invalid cross-layer drag (video↔generator)
    Blocked,
    /// Open hand: Alt over an automation segment, grabbing it to bend the
    /// curve. Distinct from the plain segment move's vertical arrows.
    Bend,
    /// Closed hand: a bend drag is in progress.
    Grabbing,
    /// Pointing hand: over an automation point (drag moves it, click deletes).
    Pointer,
}

/// Tracks the current cursor state to avoid redundant winit calls.
/// Call `set()` when interaction state changes; call `apply()` once per frame
/// to push the cursor to the window.
pub struct CursorManager {
    current: TimelineCursor,
    pending: TimelineCursor,
}

impl Default for CursorManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorManager {
    pub fn new() -> Self {
        Self {
            current: TimelineCursor::Default,
            pending: TimelineCursor::Default,
        }
    }

    /// Request a cursor change. Doesn't take effect until `apply()`.
    pub fn set(&mut self, cursor: TimelineCursor) {
        self.pending = cursor;
    }

    /// Reset to default cursor.
    pub fn set_default(&mut self) {
        self.pending = TimelineCursor::Default;
    }

    /// Returns true if the cursor changed since last apply.
    /// Call once per frame; if true, call `apply()` with the winit window.
    pub fn needs_update(&self) -> bool {
        self.current != self.pending
    }

    /// Get the pending cursor as a winit CursorIcon.
    /// Call this to get the value to pass to `window.set_cursor()`.
    pub fn pending_cursor_icon(&self) -> TimelineCursor {
        self.pending
    }

    /// Mark the pending cursor as applied.
    /// Call after `window.set_cursor()` succeeds.
    pub fn mark_applied(&mut self) {
        self.current = self.pending;
    }
}

impl From<crate::automation_hit_tester::AutomationOperation> for TimelineCursor {
    fn from(operation: crate::automation_hit_tester::AutomationOperation) -> Self {
        use crate::automation_hit_tester::AutomationOperation as Op;
        match operation {
            Op::Point => Self::Pointer,
            Op::Segment | Op::Resize => Self::ResizeVertical,
            Op::Bend => Self::Bend,
            Op::Draw => Self::Crosshair,
            Op::Insert | Op::Marquee | Op::Title => Self::Default,
            Op::Blocked => Self::Blocked,
        }
    }
}

/// The cursor for the gesture under the pointer: the operation's cursor, with
/// the bend hand closed while a bend drag is active.
impl From<&crate::automation_hit_tester::AutomationFeedback> for TimelineCursor {
    fn from(feedback: &crate::automation_hit_tester::AutomationFeedback) -> Self {
        use crate::automation_hit_tester::AutomationOperation as Op;
        if feedback.operation == Op::Bend && feedback.active {
            Self::Grabbing
        } else {
            feedback.operation.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation_hit_tester::{AutomationFeedback, AutomationOperation as Op};
    use crate::node::Vec2;
    use crate::view::UiGraphTarget;
    use manifold_foundation::{Beats, EffectId, ParamId};

    #[test]
    fn each_operation_maps_to_its_gesture_cursor() {
        let cases = [
            (Op::Point, TimelineCursor::Pointer),
            (Op::Segment, TimelineCursor::ResizeVertical),
            (Op::Bend, TimelineCursor::Bend),
            (Op::Draw, TimelineCursor::Crosshair),
            (Op::Insert, TimelineCursor::Default),
            (Op::Marquee, TimelineCursor::Default),
            (Op::Resize, TimelineCursor::ResizeVertical),
            (Op::Title, TimelineCursor::Default),
            (Op::Blocked, TimelineCursor::Blocked),
        ];
        for (op, cursor) in cases {
            assert_eq!(TimelineCursor::from(op), cursor, "{op:?}");
        }
    }

    #[test]
    fn crosshair_belongs_to_draw_alone() {
        let all = [Op::Point, Op::Segment, Op::Bend, Op::Insert, Op::Marquee, Op::Draw, Op::Resize, Op::Title, Op::Blocked];
        let crosshairs = all.iter().filter(|op| TimelineCursor::from(**op) == TimelineCursor::Crosshair).count();
        assert_eq!(crosshairs, 1);
    }

    #[test]
    fn bend_hand_closes_only_while_bending() {
        let feedback = |active| AutomationFeedback {
            target: UiGraphTarget::Effect(EffectId::new("fx")),
            param_id: ParamId::from("amount"),
            operation: Op::Bend,
            point_beat: None,
            segment_end_beat: None,
            position: Vec2::new(0.0, 0.0),
            beat: Beats(0.0),
            value: 0.0,
            active,
            hint: "",
        };
        assert_eq!(TimelineCursor::from(&feedback(false)), TimelineCursor::Bend);
        assert_eq!(TimelineCursor::from(&feedback(true)), TimelineCursor::Grabbing);
    }
}
