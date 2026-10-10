//! Native CPU payloads carried by the liquid seam's ports.

use manifold_node_engine::exec::cpu_values::CpuWireRegistration;

inventory::submit! { CpuWireRegistration::new::<crate::fluid_role::FluidRole>() }
