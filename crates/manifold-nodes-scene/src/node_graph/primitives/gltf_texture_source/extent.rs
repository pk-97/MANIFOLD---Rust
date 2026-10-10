//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.gltf_texture_source", check: texture_only }
}
