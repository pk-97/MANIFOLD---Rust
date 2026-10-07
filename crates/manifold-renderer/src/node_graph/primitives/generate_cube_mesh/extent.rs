//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, size_bounded};

inventory::submit! {
    ExtentRule { type_id: "node.cube_mesh", check: size_bounded }
}
