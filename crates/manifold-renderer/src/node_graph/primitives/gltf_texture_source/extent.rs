//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.gltf_texture_source", check: texture_only }
}
