//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{ExtentRule, particle_map};

inventory::submit! {
    ExtentRule { type_id: "node.jitter_particles", check: particle_map }
}
