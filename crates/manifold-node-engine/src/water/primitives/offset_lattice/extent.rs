//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

pub(crate) fn offset_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("levelset").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.offset_lattice", check: offset_lattice }
}
