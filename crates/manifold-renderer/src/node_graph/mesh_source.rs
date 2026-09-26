//! `MeshSource` — authored source geometry description carried on graph wires.
//!
//! This wire contains only source selectors and authored dimensions. It does
//! not carry GPU resources, solver state, transforms, or prepared proxies.

use std::sync::Arc;

use crate::node_graph::physics_mesh::MeshSelection;

/// Authored source geometry description for mesh-producing nodes.
#[derive(Clone, Debug, PartialEq)]
pub enum MeshSource {
    Cube { size: f32 },
    Platonic { shape: u32, radius: f32 },
    Gltf {
        path: Arc<str>,
        selection: MeshSelection,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::{Backend, MockBackend, NodeInputs, NodeOutputs, PortType, ResourceId};

    fn source() -> MeshSource {
        MeshSource::Gltf {
            path: Arc::from("assets/mesh.glb"),
            selection: MeshSelection {
                mesh: 2,
                primitive: 3,
                material: 4,
                fit: true,
                recenter: false,
                translate: [1.0, 2.0, 3.0],
                fragment_count: 2,
                fragment_index: 1,
                collider_parts: 7,
            },
        }
    }

    #[test]
    fn mesh_source_cpu_wire_round_trip_preserves_payload() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
        let bindings: &[(&'static str, crate::node_graph::Slot)] = &[("source", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut writes = Vec::new();
        let value = source();
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
            .with_mesh_source_writes(&mut writes);
            outputs.set_mesh_source("source", value.clone());
        }
        for (slot, value) in writes.drain(..) {
            backend.set_mesh_source(slot, value);
        }

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        assert_eq!(inputs.mesh_source("source"), Some(value));
    }

    #[test]
    fn mesh_source_clone_shares_gltf_path_arc() {
        let value = source();
        let clone = value.clone();
        let (MeshSource::Gltf { path, .. }, MeshSource::Gltf { path: cloned, .. }) =
            (&value, &clone)
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(path, cloned));
    }

    #[test]
    fn mesh_source_port_type_is_distinct_from_other_cpu_wires() {
        assert_ne!(PortType::MeshSource, PortType::FluidRole);
        assert_ne!(PortType::MeshSource, PortType::RigidBody);
        assert_ne!(PortType::MeshSource, PortType::Object);
    }

    #[test]
    fn mesh_source_release_and_clear_drop_values() {
        let mut backends: [Box<dyn Backend>; 2] = [
            Box::new(MockBackend::new()),
            Box::new(crate::node_graph::metal_backend::MetalBackend::without_device(
                1,
                1,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
            )),
        ];
        for backend in &mut backends {
            for clear in [false, true] {
                let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
                backend.set_mesh_source(slot, source());
                assert!(backend.mesh_source(slot).is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::MeshSource, None, (0, 0));
                }
                assert!(backend.mesh_source(slot).is_none());
            }
        }
    }
}
