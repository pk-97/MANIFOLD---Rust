//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn mix_arrays(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let a = x.bytes("a").ok_or_else(|| x.uncovered("a is unbound".into()))?;
    let b = x.bytes("b").ok_or_else(|| x.uncovered("b is unbound".into()))?;
    if a != b {
        return Err(Verdict::Refused(format!("Mix Arrays: input capacities must match (a={a} bytes, b={b} bytes)")));
    }
    x.covers("out", a)
}
inventory::submit! {
    ExtentRule { type_id: "node.mix_arrays", check: mix_arrays }
}
