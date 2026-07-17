//! The numeric value-cell gesture contract
//! (`docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D8, P4) — the one gesture table
//! every numeric value cell in the app's docks now speaks: drag to scrub
//! (Shift for fine), double-click to type, right-click to reset. Same shape
//! as `slider.rs`'s/`stepper.rs`'s contract (D2/D3 of
//! `UI_WIDGET_UNIFICATION_DESIGN.md`): a pure, host-agnostic `intent_for` fn;
//! hosts translate the intent into their own action type at build/input time.
//!
//! Drags don't route through the discrete [`crate::intent::Gesture`]
//! dispatcher — that enum has no `Drag` variant (drags stay host-stateful per
//! D2/D3: the existing `ValueDrag`/`DragController` sessions ARE the scrub
//! implementation). So this module owns its own [`ValueCellGesture`], covering
//! exactly the three gestures D8's table names.

/// A value cell's one interactive zone. Kept as a real (single-variant) type
/// so `intent_for`'s signature matches its `slider.rs`/`stepper.rs` siblings
/// — a numeric value cell (unlike a slider) has no separate label/track
/// split, so there is exactly one zone to speak of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueCellZone {
    Cell,
}

/// The three gestures a value cell recognizes (D8). `Drag`'s `shift` flag is
/// the only drag-time state this module needs — hosts stay in charge of the
/// pointer-tracking mechanics; this module only says what the gesture MEANS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueCellGesture {
    /// Shift held at drag-start (D8: "fine" — the host multiplies its
    /// applied delta by 0.1).
    Drag { shift: bool },
    DoubleClick,
    RightClick,
}

/// What a gesture on a value cell MEANS, in widget terms (D8's committed
/// table). Hosts translate: the scene dock / audio dock → `PanelAction`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueCellIntent {
    /// Scrub the value by drag delta. `fine`: multiply the host's applied
    /// per-pixel delta by 0.1.
    Scrub { fine: bool },
    /// Open the type-in box (double-click to type).
    EditValue,
    /// Write the cell's default back through its value path.
    ResetToDefault,
}

pub struct ValueCell;

impl ValueCell {
    /// The gesture contract (D8). Pure, total, allocation-free. Owns exactly
    /// three (zone, gesture) pairs — the same total-coverage shape as
    /// `slider.rs`'s/`stepper.rs`'s `intent_for`, just with only one zone to
    /// range over.
    pub fn intent_for(_zone: ValueCellZone, g: ValueCellGesture) -> Option<ValueCellIntent> {
        match g {
            ValueCellGesture::Drag { shift } => Some(ValueCellIntent::Scrub { fine: shift }),
            ValueCellGesture::DoubleClick => Some(ValueCellIntent::EditValue),
            ValueCellGesture::RightClick => Some(ValueCellIntent::ResetToDefault),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins D8's full contract table — the one place the table itself is
    /// asserted, so a change here is a deliberate contract edit, not drift.
    #[test]
    fn intent_for_pins_the_full_contract_table() {
        assert_eq!(
            ValueCell::intent_for(ValueCellZone::Cell, ValueCellGesture::Drag { shift: false }),
            Some(ValueCellIntent::Scrub { fine: false })
        );
        assert_eq!(
            ValueCell::intent_for(ValueCellZone::Cell, ValueCellGesture::Drag { shift: true }),
            Some(ValueCellIntent::Scrub { fine: true })
        );
        assert_eq!(
            ValueCell::intent_for(ValueCellZone::Cell, ValueCellGesture::DoubleClick),
            Some(ValueCellIntent::EditValue)
        );
        assert_eq!(
            ValueCell::intent_for(ValueCellZone::Cell, ValueCellGesture::RightClick),
            Some(ValueCellIntent::ResetToDefault)
        );
    }

    #[test]
    fn fine_flag_rides_the_shift_flag_unchanged() {
        for shift in [false, true] {
            assert_eq!(
                ValueCell::intent_for(ValueCellZone::Cell, ValueCellGesture::Drag { shift }),
                Some(ValueCellIntent::Scrub { fine: shift })
            );
        }
    }
}
