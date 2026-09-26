//! `FluidRole` — CPU payload carried on [`super::ports::PortType::FluidRole`] wires.
//!
//! A fluid role owns immutable prepared local-space geometry plus the live
//! authored controls consumed by the fluid runtime. Geometry remains an
//! `Arc` so graph execution can pass it through the CPU wire without cloning
//! mesh data.

use std::sync::Arc;

use manifold_physics::TriangleMesh;

use super::transform::Transform;

/// Maximum number of fluid-role ports supported by a graph boundary.
pub const MAX_FLUID_ROLES: usize = 64;

/// Semantic role a prepared geometry source contributes to the fluid solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FluidRoleKind {
    InitialFill,
    Inflow,
    Outflow,
    Collider,
}

/// Immutable prepared local-space geometry for a fluid role.
#[derive(Debug)]
pub struct PreparedFluidGeometry {
    pub meshes: Vec<TriangleMesh>,
}

/// CPU payload carried by a [`super::ports::PortType::FluidRole`] wire.
#[derive(Clone, Debug)]
pub struct FluidRole {
    pub geometry: Arc<PreparedFluidGeometry>,
    pub kind: FluidRoleKind,
    pub transform: Transform,
    pub enabled: bool,
    pub velocity: [f32; 3],
    pub inherit_motion: f32,
    pub friction: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::{Backend, MockBackend, NodeInputs, NodeOutputs, PortType, ResourceId};

    fn role() -> FluidRole {
        FluidRole {
            geometry: Arc::new(PreparedFluidGeometry {
                meshes: vec![TriangleMesh {
                    vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    triangles: vec![[0, 1, 2]],
                }],
            }),
            kind: FluidRoleKind::Inflow,
            transform: Transform {
                pos: [1.0, 2.0, 3.0],
                ..Transform::default()
            },
            enabled: true,
            velocity: [4.0, 5.0, 6.0],
            inherit_motion: 0.25,
            friction: 0.75,
        }
    }

    #[test]
    fn scene_physics_fluid_role_cpu_wire_round_trip_preserves_arc_and_values() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::FluidRole, None, (0, 0));
        let bindings: &[(&'static str, crate::node_graph::Slot)] = &[("role", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut fluid_role_writes = Vec::new();
        let value = role();
        let geometry = Arc::clone(&value.geometry);
        {
            let mut outputs = NodeOutputs::new(
                bindings,
                &backend,
                &mut scalar,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            )
            .with_fluid_role_writes(&mut fluid_role_writes);
            outputs.set_fluid_role("role", value.clone());
        }

        for (slot, value) in fluid_role_writes.drain(..) {
            backend.set_fluid_role(slot, value);
        }

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        let got = inputs
            .fluid_role("role")
            .expect("fluid role should be wired");
        assert!(Arc::ptr_eq(&got.geometry, &geometry));
        assert_eq!(got.kind, value.kind);
        assert_eq!(got.transform, value.transform);
        assert_eq!(got.enabled, value.enabled);
        assert_eq!(got.velocity, value.velocity);
        assert_eq!(got.inherit_motion, value.inherit_motion);
        assert_eq!(got.friction, value.friction);
    }

    #[test]
    fn scene_physics_fluid_role_port_type_is_distinct_from_other_cpu_wires() {
        assert_ne!(PortType::FluidRole, PortType::RigidBody);
        assert_ne!(PortType::FluidRole, PortType::Object);
        assert_ne!(PortType::FluidRole, PortType::Transform);
    }

    #[test]
    fn scene_physics_fluid_role_release_and_clear_drop_geometry() {
        let mut backends: [Box<dyn Backend>; 2] = [
            Box::new(MockBackend::new()),
            Box::new(
                crate::node_graph::metal_backend::MetalBackend::without_device(
                    1,
                    1,
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                ),
            ),
        ];
        for backend in &mut backends {
            for clear in [false, true] {
                let value = role();
                let weak = Arc::downgrade(&value.geometry);
                let slot = backend.acquire(ResourceId(0), PortType::FluidRole, None, (0, 0));
                backend.set_fluid_role(slot, value);
                assert!(weak.upgrade().is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::FluidRole, None, (0, 0));
                }
                assert!(backend.fluid_role(slot).is_none());
                assert!(
                    weak.upgrade().is_none(),
                    "released scene geometry must not stay retained in the slot map"
                );
            }
        }
    }
}
