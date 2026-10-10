//! Native CPU payloads carried by the graph's water ports.

use manifold_node_engine::exec::cpu_values::CpuWireRegistration;

inventory::submit! { CpuWireRegistration::new::<super::physics::RigidBody>() }
inventory::submit! { CpuWireRegistration::new::<super::fluid_role::FluidRole>() }
inventory::submit! { CpuWireRegistration::new::<manifold_physics::FieldValue>() }

#[cfg(test)]
mod tests {
    use manifold_node_engine::exec::backend::{Backend, MockBackend};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::exec::metal_backend::MetalBackend;
    use manifold_node_engine::ports::PortType;
    use crate::physics::RigidBody;

    #[test]
    fn recycled_rigid_body_slots_do_not_publish_previous_values() {
        let mut backends: [Box<dyn Backend>; 2] = [
            Box::new(MockBackend::new()),
            Box::new(MetalBackend::without_device(1, 1, manifold_gpu::GpuTextureFormat::Rgba16Float)),
        ];
        for backend in &mut backends {
            for reset in [false, true] {
                let slot = backend.acquire(ResourceId(0), PortType::RigidBody, None, (0, 0));
                backend.cpu_values_mut().set(slot, RigidBody::default());
                assert!(backend.cpu_values().get::<RigidBody>(slot).is_some());
                if reset {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::RigidBody, None, (0, 0));
                    let recycled = backend.acquire(ResourceId(1), PortType::RigidBody, None, (0, 0));
                    assert_eq!(recycled, slot);
                }
                assert!(backend.cpu_values().get::<RigidBody>(slot).is_none());
            }
        }
    }
}
