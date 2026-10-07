//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, size_bounded};

inventory::submit! {
    ExtentRule { type_id: "node.projected_grid", check: size_bounded }
}
