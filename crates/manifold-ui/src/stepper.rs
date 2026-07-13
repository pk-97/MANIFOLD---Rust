//! The gain-stepper widget's gesture contract (UI_WIDGET_UNIFICATION_DESIGN.md
//! P2). A `[−]value[＋]` control — `audio_setup_panel.rs`'s per-send gain row
//! is the sole instance today, and its stepper buttons and the D7
//! overlay-drag calibration both drive the SAME underlying gain value, so
//! there is exactly one live gesture to contract: right-click anywhere on the
//! control resets to the widget's declared default (unity, 0 dB — BUG-070's
//! remainder). Same shape as `slider.rs`'s contract (D2: per-widget inherent
//! fns, no trait/framework — a widget count this small doesn't earn one)
//! deliberately mirrored so both read as one pattern, not two.
//!
//! Unlike the card/canvas slider hosts, `AudioSetupPanel` doesn't route its
//! own gestures through `IntentRegistry` at all (it owns a single
//! `handle_event` dispatcher covering drags, dividers, and clicks together) —
//! migrating that dispatch onto the registry is a panel-wide architecture
//! change, not a single-widget contract's remit, so P2 stops at "the gesture
//! MEANING lives in the widget" (this module) and leaves the panel's existing
//! dispatch mechanism as the translation point, exactly as D3 allows a host
//! to translate however its surface already works.

use crate::intent::Gesture;

/// A stepper's interactive zones. All three currently resolve to the same
/// intent (see [`Stepper::intent_for`]) — the widget's whole footprint reads
/// as "one control," matching the live behaviour (BUG-070) exactly, not
/// three independently-clickable targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepperZone {
    Minus,
    Value,
    Plus,
}

/// What a gesture on a zone MEANS, in widget terms (D2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepperIntent {
    /// Write the widget's default back through the value path, undoable as
    /// one drag (D4) — the panel replays its `*DragBegin/*DragChanged(0.0)/
    /// *DragCommit` trio, mirroring `PanelAction::slider_reset`.
    ResetToDefault,
}

pub struct Stepper;

impl Stepper {
    /// The gesture contract. Pure, total, allocation-free.
    pub fn intent_for(_zone: StepperZone, g: Gesture) -> Option<StepperIntent> {
        match g {
            Gesture::RightClick => Some(StepperIntent::ResetToDefault),
            Gesture::Click | Gesture::DoubleClick => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_zone_right_click_resets() {
        for zone in [StepperZone::Minus, StepperZone::Value, StepperZone::Plus] {
            assert_eq!(Stepper::intent_for(zone, Gesture::RightClick), Some(StepperIntent::ResetToDefault));
            assert_eq!(Stepper::intent_for(zone, Gesture::Click), None);
            assert_eq!(Stepper::intent_for(zone, Gesture::DoubleClick), None);
        }
    }
}
