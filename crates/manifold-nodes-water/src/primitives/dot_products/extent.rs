//! Buffer extent rule owned by this node.
use crate::primitives::dot_products::MAX_ROWS;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict, whole_param};

fn dot_products(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let length = u64::from(whole_param(x, "row_length", 1024.0).max(1));
    let max_rows = whole_param(x, "max_rows", 1.0);
    if !(1..=MAX_ROWS).contains(&max_rows) {
        return Err(x.uncovered(format!("max_rows {max_rows} is outside the partials' 1 to {MAX_ROWS} rows")));
    }
    let max_rows = u64::from(max_rows);
    x.covers("matrix", max_rows * length * 4)?;
    if x.wired("vector") {
        x.covers("vector", length * 4)?;
    }
    x.covers("out", max_rows * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.dot_products", check: dot_products }
}
