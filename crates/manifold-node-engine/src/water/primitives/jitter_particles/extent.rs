//! Buffer extent rule owned by this node.
use crate::exec::extent::{ExtentRule};
use crate::water::liquid::extent::{particle_map};

inventory::submit! {
    ExtentRule { type_id: "node.jitter_particles", check: particle_map }
}
