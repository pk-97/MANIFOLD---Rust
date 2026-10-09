//! Buffer extent rule owned by this node.
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};

pub(crate) fn emission_count(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let values = x.items("particles").unwrap_or(0) * 4;
    x.covers("energy", values)?;
    x.covers("wavecrest", values)?;
    x.covers("out", values)
}
inventory::submit! {
    ExtentRule { type_id: "node.emission_count", check: emission_count }
}
