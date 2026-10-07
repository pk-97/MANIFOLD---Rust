//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn push_out_of_solid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, bytes) = crate::node_graph::primitives::push_out_of_solid::solid_shape(|name, default| x.scalar(name, default))
        .map_err(Verdict::Refused)?;
    x.covers("solid", bytes)?;
    let particles = x.bytes("particles").ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    x.covers("out", particles)
}
inventory::submit! {
    ExtentRule { type_id: "node.push_out_of_solid", check: push_out_of_solid }
}
