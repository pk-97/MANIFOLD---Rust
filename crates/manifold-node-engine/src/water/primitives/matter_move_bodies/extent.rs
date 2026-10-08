//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn matter_move_bodies(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // Rows and bodies clamp to the bodies arrays; the reaction is read only
    // when it covers every body.
    x.covers("bodies_out", size_of::<LiquidBody>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_move_bodies", check: matter_move_bodies }
}
