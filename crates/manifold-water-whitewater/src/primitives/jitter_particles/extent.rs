//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::ExtentRule;
use manifold_water_liquid::extent::particle_map;

inventory::submit! {
    ExtentRule { type_id: "node.jitter_particles", check: particle_map }
}
