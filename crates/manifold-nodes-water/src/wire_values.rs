//! Native CPU payloads carried by the graph's water ports.

use manifold_node_engine::exec::cpu_values::CpuWireRegistration;

inventory::submit! { CpuWireRegistration::new::<super::fluid_role::FluidRole>() }
