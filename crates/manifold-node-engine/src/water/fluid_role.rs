//! `FluidRole` — CPU payload carried on [`super::ports::PortType::FluidRole`] wires.
//!
//! A fluid role owns immutable prepared local-space geometry plus the live
//! authored controls consumed by the fluid runtime. Geometry remains an
//! `Arc` so graph execution can pass it through the CPU wire without cloning
//! mesh data.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use manifold_physics::TriangleMesh;
use manifold_physics::sdf::{DistanceLattice, signed_distance_union};

use crate::scene::transform::Transform;


/// Semantic role a prepared geometry source contributes to the fluid solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FluidRoleKind {
    InitialFill,
    Inflow,
    Outflow,
    Collider,
}

/// Distance lattice nodes along the longest local axis of a role's geometry.
/// About 34³ nodes with padding: under a second of CPU for a 1,000-triangle
/// mesh on the worker (GPU_MPM_SOLVER_DESIGN.md D16).
pub const DISTANCE_NODES_ALONG_LONGEST: f32 = 32.0;

/// Immutable prepared local-space geometry for a fluid role.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFluidGeometry {
    pub meshes: Vec<TriangleMesh>,
    /// Derived, never serialized: a take or cache identity hashes the meshes
    /// only (GPU_MPM_SOLVER_DESIGN.md D11).
    #[serde(skip)]
    distance: DerivedDistance,
}

#[derive(Debug, Default)]
struct DerivedDistance {
    requested: AtomicBool,
    lattice: OnceLock<Result<Arc<DistanceLattice>, String>>,
}

/// Where a role's signed-distance lattice stands.
#[derive(Clone, Debug)]
pub enum DistanceState {
    Pending,
    Ready(Arc<DistanceLattice>),
    Failed(String),
}

impl PreparedFluidGeometry {
    pub fn new(meshes: Vec<TriangleMesh>) -> Self {
        Self { meshes, distance: DerivedDistance::default() }
    }

    /// The body-local signed-distance lattice of the union of this
    /// geometry's meshes, unscaled (D11; `signed_distance_union`). The first
    /// call starts the build on a worker thread and returns Pending; FLIP,
    /// which never asks, pays nothing.
    pub fn distance_lattice(self: &Arc<Self>) -> DistanceState {
        if let Some(result) = self.distance.lattice.get() {
            return match result {
                Ok(lattice) => DistanceState::Ready(Arc::clone(lattice)),
                Err(error) => DistanceState::Failed(error.clone()),
            };
        }
        if !self.distance.requested.swap(true, Ordering::AcqRel) {
            let geometry = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("fluid-role-distance".into())
                .spawn(move || {
                    // Always settle the lattice, so a waiter never waits on a
                    // build that panicked.
                    let built = std::panic::catch_unwind(|| build_distance(&geometry.meshes))
                        .unwrap_or_else(|_| Err("Fluid role distance lattice build panicked".into()));
                    let _ = geometry.distance.lattice.set(built);
                });
            if let Err(error) = spawned {
                let _ = self
                    .distance
                    .lattice
                    .set(Err(format!("Fluid role distance lattice could not start: {error}")));
            }
        }
        DistanceState::Pending
    }

    /// [`Self::distance_lattice`], waiting for the build instead of returning
    /// Pending: an offline render never spends simulated time on it.
    pub fn wait_distance_lattice(self: &Arc<Self>) -> DistanceState {
        match self.distance_lattice() {
            DistanceState::Pending => match self.distance.lattice.wait() {
                Ok(lattice) => DistanceState::Ready(Arc::clone(lattice)),
                Err(error) => DistanceState::Failed(error.clone()),
            },
            settled => settled,
        }
    }
}

/// One lattice over every mesh, the union of their signed distances.
fn build_distance(meshes: &[TriangleMesh]) -> Result<Arc<DistanceLattice>, String> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in meshes.iter().flat_map(|mesh| &mesh.vertices) {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    let longest = (0..3).map(|axis| max[axis] - min[axis]).fold(0.0f32, f32::max);
    if !(longest.is_finite() && longest > 0.0) {
        return Err("Fluid role geometry has no extent for a distance lattice".into());
    }
    let spacing = longest / DISTANCE_NODES_ALONG_LONGEST;
    signed_distance_union(meshes, spacing, 2.0 * spacing)
        .map(Arc::new)
        .map_err(|error| format!("Fluid role distance lattice: {error}"))
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
    use crate::{exec::backend::Backend, exec::backend::MockBackend, exec::cpu_values::CpuWireWrites, bindings::NodeInputs, bindings::NodeOutputs, ports::PortType, exec::execution_plan::ResourceId};

    fn role() -> FluidRole {
        FluidRole {
            geometry: Arc::new(PreparedFluidGeometry {
                meshes: vec![TriangleMesh {
                    vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    triangles: vec![[0, 1, 2]],
                }],
                ..Default::default()
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
        let bindings: &[(&'static str, crate::bindings::Slot)] = &[("role", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut fluid_role_writes = CpuWireWrites::default();
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
            .with_cpu_value_writes(&mut fluid_role_writes);
            outputs.set_cpu_value("role", value.clone());
        }

        fluid_role_writes.commit(backend.cpu_values_mut());

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        let got = inputs
            .cpu_value::<FluidRole>("role")
            .expect("fluid role should be wired");
        assert!(Arc::ptr_eq(&got.geometry, &geometry));
        assert_eq!(got.kind, value.kind);
        assert_eq!(got.transform, value.transform);
        assert_eq!(got.enabled, value.enabled);
        assert_eq!(got.velocity, value.velocity);
        assert_eq!(got.inherit_motion, value.inherit_motion);
        assert_eq!(got.friction, value.friction);
    }

    /// A unit cube split in two closed halves, as a two-mesh compound role.
    fn two_part_cube() -> PreparedFluidGeometry {
        let cuboid = |x0: f32, x1: f32| {
            let v = |x: usize, y: usize, z: usize| [[x0, x1][x], [-0.5, 0.5][y], [-0.5, 0.5][z]];
            TriangleMesh {
                vertices: vec![
                    v(0, 0, 0), v(1, 0, 0), v(1, 1, 0), v(0, 1, 0),
                    v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1),
                ],
                triangles: vec![
                    [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                    [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
                    [1, 2, 6], [1, 6, 5], [0, 4, 7], [0, 7, 3],
                ],
            }
        };
        PreparedFluidGeometry::new(vec![cuboid(-0.5, 0.0), cuboid(0.0, 0.5)])
    }

    fn wait_for_distance(geometry: &Arc<PreparedFluidGeometry>) -> Arc<DistanceLattice> {
        let start = std::time::Instant::now();
        loop {
            match geometry.distance_lattice() {
                DistanceState::Ready(lattice) => return lattice,
                DistanceState::Failed(error) => panic!("{error}"),
                DistanceState::Pending => {
                    assert!(start.elapsed().as_secs() < 30, "the distance lattice never arrived");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    /// One lattice spans every mesh of the role, spacing = longest extent / 32
    /// with two spacings of padding. Outside the two halves it is the cube's
    /// exact distance; inside, each part's own distance (a bound on the
    /// union's depth, 0 on the faces where the halves touch) with the sign of
    /// the union.
    #[test]
    fn scene_physics_fluid_role_distance_lattice_unions_meshes() {
        let geometry = Arc::new(two_part_cube());
        let lattice = wait_for_distance(&geometry);
        assert_eq!(lattice.spacing, 1.0 / DISTANCE_NODES_ALONG_LONGEST);
        assert_eq!(lattice.dims, [37; 3]);
        // Node 10 is x = −0.25, the middle of the left half.
        assert!((lattice.value([10, 18, 18]) + 0.25).abs() < 1e-5, "{}", lattice.value([10, 18, 18]));
        assert!(lattice.value([18, 18, 18]).abs() < 1e-5, "on the touching faces");
        assert!((lattice.value([0, 18, 18]) - 2.0 / 32.0).abs() < 1e-5);
        assert!((lattice.value([0, 0, 18]) - (2.0f32 * (2.0 / 32.0) * (2.0 / 32.0)).sqrt()).abs() < 1e-5);
        assert!(Arc::ptr_eq(&lattice, &wait_for_distance(&geometry)), "built once");
    }

    /// The lattice is derived: serializing the geometry (what a physics take
    /// hashes) gives the same bytes before and after it exists.
    #[test]
    fn scene_physics_fluid_role_distance_lattice_is_not_serialized() {
        let geometry = Arc::new(two_part_cube());
        let before = serde_json::to_vec(&*geometry).unwrap();
        wait_for_distance(&geometry);
        assert_eq!(before, serde_json::to_vec(&*geometry).unwrap());
        let restored: PreparedFluidGeometry = serde_json::from_slice(&before).unwrap();
        assert_eq!(restored.meshes, geometry.meshes);
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
                crate::exec::metal_backend::MetalBackend::without_device(
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
                backend.cpu_values_mut().set(slot, value);
                assert!(weak.upgrade().is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::FluidRole, None, (0, 0));
                }
                assert!(backend.cpu_values().get::<FluidRole>(slot).is_none());
                assert!(
                    weak.upgrade().is_none(),
                    "released scene geometry must not stay retained in the slot map"
                );
            }
        }
    }
}
