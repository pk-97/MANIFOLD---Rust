//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{ExtentRule, particle_map};

inventory::submit! {
    ExtentRule { type_id: "node.jitter_particles", check: particle_map }
}
