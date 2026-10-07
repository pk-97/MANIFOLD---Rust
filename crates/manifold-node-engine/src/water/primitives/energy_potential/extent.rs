//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{ExtentRule, particle_values};

inventory::submit! {
    ExtentRule { type_id: "node.energy_potential", check: particle_values }
}
