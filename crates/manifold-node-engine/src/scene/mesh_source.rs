//! `MeshSource` — authored source geometry description carried on graph wires.
//!
//! This wire contains only source selectors and authored dimensions. It does
//! not carry GPU resources, solver state, transforms, or prepared proxies.

use std::sync::Arc;

use crate::mesh::{MeshVertex, PLATONIC_SHAPES};
use crate::platonic::platonic_mesh;
use crate::scene::mesh_selection::MeshSelection;

/// Authored source geometry description for mesh-producing nodes.
#[derive(Clone, Debug, PartialEq)]
pub enum MeshSource {
    Cube { size: f32 },
    Platonic { shape: u32, radius: f32 },
    /// `node.plane_mesh`: the XY plane facing +Z, `width` by `height`.
    Plane { width: f32, height: f32 },
    Gltf {
        path: Arc<str>,
        /// The one file node whose mesh this is (an imported part), or `None`
        /// for what `selection` picks from the whole file.
        node_path: Option<Arc<str>>,
        selection: MeshSelection,
    },
}

impl MeshSource {
    /// Load the immutable, local-space triangle-list vertices described by this
    /// source.  Selectors are validated here so every geometry consumer uses
    /// the same source contract before applying its own fixed transform.
    pub fn load_vertices(&self) -> Result<Vec<MeshVertex>, String> {
        let (shape, radius) = match self {
            Self::Plane { width, height } => return plane_vertices(*width, *height),
            Self::Cube { size } => {
                if !size.is_finite() || *size <= 0.0 {
                    return Err("mesh source size must be finite and positive".into());
                }
                (1, *size * 3.0_f32.sqrt() / 2.0)
            }
            Self::Platonic { shape, radius } => {
                if !radius.is_finite() || *radius <= 0.0 {
                    return Err("mesh source size must be finite and positive".into());
                }
                (*shape, *radius)
            }
            Self::Gltf { path, node_path, selection } => {
                if path.is_empty() {
                    return Err("mesh source has no file".into());
                }
                if selection.translate.iter().any(|value| !value.is_finite()) {
                    return Err("mesh source offsets must be finite".into());
                }
                if !(1..=64).contains(&selection.fragment_count)
                    || selection.fragment_index >= selection.fragment_count
                    || selection.mesh < -1
                    || selection.primitive < -1
                    || selection.material < -2
                {
                    return Err("mesh source has invalid selectors".into());
                }
                return selection.load_node(std::path::Path::new(path.as_ref()), node_path.as_deref());
            }
        };
        if shape >= PLATONIC_SHAPES.len() as u32 {
            return Err("mesh source has an invalid builtin shape".into());
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err("mesh source size must be finite and positive".into());
        }
        let mut vertices = platonic_mesh(shape).to_vec();
        for vertex in &mut vertices {
            for axis in 0..3 {
                vertex.position[axis] *= radius;
            }
        }
        Ok(vertices)
    }
}

/// The two triangles of `node.plane_mesh`: the XY plane facing +Z.
fn plane_vertices(width: f32, height: f32) -> Result<Vec<MeshVertex>, String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("mesh source size must be finite and positive".into());
    }
    let (x, y) = (width * 0.5, height * 0.5);
    let corner = |px: f32, py: f32| {
        let mut vertex: MeshVertex = bytemuck::Zeroable::zeroed();
        vertex.position = [px, py, 0.0];
        vertex.normal = [0.0, 0.0, 1.0];
        vertex.uv = [px / width + 0.5, 0.5 - py / height];
        vertex.tangent = [1.0, 0.0, 0.0, 1.0];
        vertex.color = [1.0; 4];
        vertex
    };
    Ok(vec![corner(-x, -y), corner(x, -y), corner(x, y), corner(-x, -y), corner(x, y), corner(-x, y)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{exec::backend::Backend, exec::backend::MockBackend, bindings::NodeInputs, bindings::NodeOutputs, ports::PortType, exec::execution_plan::ResourceId};

    fn source() -> MeshSource {
        MeshSource::Gltf {
            path: Arc::from("assets/mesh.glb"),
            node_path: None,
            selection: MeshSelection {
                mesh: 2,
                primitive: 3,
                material: 4,
                fit: true,
                recenter: false,
                translate: [1.0, 2.0, 3.0],
                fragment_count: 2,
                fragment_index: 1,
            },
        }
    }

    #[test]
    fn mesh_source_cpu_wire_round_trip_preserves_payload() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
        let bindings: &[(&'static str, crate::bindings::Slot)] = &[("source", slot)];
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
    fn mesh_source_cube_size_matches_visible_bounds() {
        let vertices = MeshSource::Cube { size: 2.0 }.load_vertices().unwrap();
        let extent = vertices
            .iter()
            .map(|vertex| vertex.position[0].abs())
            .fold(0.0_f32, f32::max);
        assert!((extent - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn mesh_source_platonic_shape_and_radius_are_applied() {
        let vertices = MeshSource::Platonic {
            shape: 0,
            radius: 2.5,
        }
        .load_vertices()
        .unwrap();
        assert_eq!(vertices.len(), 12);
        let radius = vertices
            .iter()
            .map(|vertex| {
                vertex
                    .position
                    .iter()
                    .map(|value| value * value)
                    .sum::<f32>()
                    .sqrt()
            })
            .fold(0.0_f32, f32::max);
        assert!((radius - 2.5).abs() < 1.0e-6);
    }

    #[test]
    fn mesh_source_rejects_invalid_dimensions_and_selectors() {
        assert!(MeshSource::Cube { size: 0.0 }.load_vertices().is_err());
        assert!(MeshSource::Platonic {
            shape: PLATONIC_SHAPES.len() as u32,
            radius: 1.0,
        }
        .load_vertices()
        .is_err());
        assert!(MeshSource::Gltf {
            path: Arc::from("assets/mesh.glb"),
            node_path: None,
            selection: MeshSelection {
                mesh: -1,
                primitive: -1,
                material: -1,
                fit: false,
                recenter: true,
                translate: [0.0; 3],
                fragment_count: 1,
                fragment_index: 1,
            },
        }
        .load_vertices()
        .is_err());
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
            Box::new(crate::exec::metal_backend::MetalBackend::without_device(
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
