//! Buffer extent rule owned by this node.
use crate::exec::extent::{ExtentRule};
use crate::water::liquid::extent::{particle_values};

inventory::submit! {
    ExtentRule { type_id: "node.energy_potential", check: particle_values }
}
