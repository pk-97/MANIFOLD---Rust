//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, whole};

inventory::submit! {
    ExtentRule { type_id: "node.upwind_distance", check: |x| {
        let count=["nodes_x","nodes_y","nodes_z"].map(|p|u64::from(whole(x,p,8.0))).into_iter().product::<u64>();
        for port in ["levelset","valid","out"] { x.covers(port,count*4)?; } Ok(())
    } }
}
