//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.motion_blur", check: texture_only }
}
