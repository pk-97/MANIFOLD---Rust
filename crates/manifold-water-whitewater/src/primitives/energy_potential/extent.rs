//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::ExtentRule;
use manifold_water_liquid::extent::particle_values;

inventory::submit! {
    ExtentRule { type_id: "node.energy_potential", check: particle_values }
}
