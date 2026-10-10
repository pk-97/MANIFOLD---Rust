//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict, size_bounded};

fn ocean_displace(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    for (param, port) in [("size_0", "field_0"), ("size_1", "field_1"), ("size_2", "field_2")] {
        let size = x.param(param, 256.0).round();
        let n = size as u32;
        if !(16.0..=1024.0).contains(&size) || !n.is_power_of_two() {
            return Err(Verdict::Refused(format!("Ocean Displace: {param} must be a power of two in 16..1024 (got {size})")));
        }
        let n = u64::from(n);
        // Wrapped gathers reach all six N × N real fields of each cascade.
        x.covers(port, 6 * n * n * 4)?;
    }
    // The vertex dispatch clamps to min(mesh.size, out.size).
    size_bounded(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.ocean_displace", check: ocean_displace }
}
