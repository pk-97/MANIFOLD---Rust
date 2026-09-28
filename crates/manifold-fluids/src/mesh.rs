use manifold_physics::{BodyPose, TriangleMesh};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{FluidError, FluidWorld};

static NEXT_MESH_PROVENANCE: AtomicU64 = AtomicU64::new(1);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshRole {
    Inflow = 0,
    Outflow = 1,
    Collider = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InflowOptions {
    pub velocity: [f32; 3],
    pub inherit_motion: f32,
}

impl Default for InflowOptions {
    fn default() -> Self {
        Self {
            velocity: [0.0; 3],
            inherit_motion: 0.0,
        }
    }
}

impl InflowOptions {
    fn validate(self) -> Result<(), FluidError> {
        if self.velocity.iter().any(|value| !value.is_finite()) {
            return Err(FluidError::input("inflow velocity must be finite"));
        }
        if !self.inherit_motion.is_finite() || self.inherit_motion < 0.0 {
            return Err(FluidError::input(
                "inflow inherit_motion must be finite and non-negative",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshHandle {
    provenance: u64,
    slot: u32,
    generation: u64,
}

struct MeshSlot {
    generation: u64,
    role: Option<MeshRole>,
    previous: [f32; 7],
    current: [f32; 7],
    next: [f32; 7],
}

pub(crate) struct MeshState {
    provenance: u64,
    slots: Vec<MeshSlot>,
    free: Vec<u32>,
}

impl MeshState {
    pub(crate) fn new() -> Result<Self, FluidError> {
        let provenance = NEXT_MESH_PROVENANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| FluidError::native("fluid mesh handle provenance exhausted"))?;
        Ok(Self {
            provenance,
            slots: Vec::new(),
            free: Vec::new(),
        })
    }

    pub(super) fn validate_handle(
        &self,
        handle: MeshHandle,
        role: Option<MeshRole>,
    ) -> Result<usize, FluidError> {
        if handle.provenance != self.provenance {
            return Err(FluidError::native(
                "mesh handle does not belong to this world",
            ));
        }
        let slot = self
            .slots
            .get(handle.slot as usize)
            .ok_or_else(|| FluidError::native("mesh handle is stale or invalid"))?;
        if slot.generation != handle.generation {
            return Err(FluidError::native("mesh handle is stale or invalid"));
        }
        let actual = slot
            .role
            .ok_or_else(|| FluidError::native("mesh handle is stale or invalid"))?;
        if role.is_some_and(|expected| expected != actual) {
            return Err(FluidError::native(
                "mesh handle role does not match operation",
            ));
        }
        Ok(handle.slot as usize)
    }

    fn allocate(&mut self, role: MeshRole, pose: [f32; 7]) -> Result<MeshHandle, FluidError> {
        while let Some(slot) = self.free.pop() {
            let record = &mut self.slots[slot as usize];
            if record.generation == u64::MAX {
                continue;
            }
            record.role = Some(role);
            record.previous = pose;
            record.current = pose;
            record.next = pose;
            return Ok(MeshHandle {
                provenance: self.provenance,
                slot,
                generation: record.generation,
            });
        }
        let slot = u32::try_from(self.slots.len())
            .map_err(|_| FluidError::input("too many mesh roles in one fluid world"))?;
        self.slots.push(MeshSlot {
            generation: 1,
            role: Some(role),
            previous: pose,
            current: pose,
            next: pose,
        });
        Ok(MeshHandle {
            provenance: self.provenance,
            slot,
            generation: 1,
        })
    }

    fn release_after_remove(&mut self, slot: usize) {
        let record = &mut self.slots[slot];
        record.role = None;
        if record.generation != u64::MAX {
            record.generation += 1;
            self.free.push(slot as u32);
        }
    }
}

impl FluidWorld {
    /// Register a transformed mesh source, drain, or collider.
    pub fn add_mesh(
        &mut self,
        mesh: &TriangleMesh,
        role: MeshRole,
        pose: BodyPose,
    ) -> Result<MeshHandle, FluidError> {
        validate_mesh(mesh)?;
        let pose = normalize_pose(pose)?;
        validate_transformed_mesh(mesh, pose)?;
        let (vertices, triangles) = flatten_mesh(mesh)?;
        let handle = self.mesh_state.allocate(role, pose)?;
        let ok = unsafe {
            super::manifold_fluids_world_add_mesh(
                self.native,
                handle.slot,
                role as u8,
                vertices.as_ptr(),
                mesh.vertices.len(),
                triangles.as_ptr(),
                mesh.triangles.len(),
                pose.as_ptr(),
            )
        };
        if let Err(error) = super::native_result(ok, "adding a fluid mesh") {
            self.mesh_state.slots[handle.slot as usize].role = None;
            self.mesh_state.free.push(handle.slot);
            return Err(error);
        }
        Ok(handle)
    }

    /// Add an initial liquid volume. The native solver copies this mesh and does not retain a handle.
    pub fn add_fluid_mesh(
        &mut self,
        mesh: &TriangleMesh,
        pose: BodyPose,
        velocity: [f32; 3],
    ) -> Result<(), FluidError> {
        validate_mesh(mesh)?;
        let pose = normalize_pose(pose)?;
        validate_transformed_mesh(mesh, pose)?;
        if velocity.iter().any(|value| !value.is_finite()) {
            return Err(FluidError::input(
                "initial fluid mesh velocity must be finite",
            ));
        }
        let (vertices, triangles) = flatten_mesh(mesh)?;
        let ok = unsafe {
            super::manifold_fluids_world_add_fluid_mesh(
                self.native,
                vertices.as_ptr(),
                mesh.vertices.len(),
                triangles.as_ptr(),
                mesh.triangles.len(),
                pose.as_ptr(),
                velocity.as_ptr(),
            )
        };
        super::native_result(ok, "adding an initial fluid mesh")
    }

    pub fn set_mesh_motion(
        &mut self,
        handle: MeshHandle,
        previous: BodyPose,
        current: BodyPose,
        next: BodyPose,
    ) -> Result<(), FluidError> {
        let slot = self.mesh_state.validate_handle(handle, None)?;
        let previous = normalize_pose(previous)?;
        let current = normalize_pose(current)?;
        let next = normalize_pose(next)?;
        let record = &self.mesh_state.slots[slot];
        if record.previous == previous && record.current == current && record.next == next {
            return Ok(());
        }
        // The native side owns the immutable geometry. Validate every pose against it
        // before mutating either side of the bridge.
        // Rust retains no geometry copy, so the native bridge repeats its overflow guard.
        let ok = unsafe {
            super::manifold_fluids_world_set_mesh_motion(
                self.native,
                handle.slot,
                previous.as_ptr(),
                current.as_ptr(),
                next.as_ptr(),
            )
        };
        super::native_result(ok, "setting fluid mesh motion")?;
        let record = &mut self.mesh_state.slots[slot];
        record.previous = previous;
        record.current = current;
        record.next = next;
        Ok(())
    }

    pub fn set_mesh_enabled(
        &mut self,
        handle: MeshHandle,
        enabled: bool,
    ) -> Result<(), FluidError> {
        self.mesh_state.validate_handle(handle, None)?;
        let ok = unsafe {
            super::manifold_fluids_world_set_mesh_enabled(
                self.native,
                handle.slot,
                i32::from(enabled),
            )
        };
        super::native_result(ok, "setting fluid mesh enabled state")?;
        Ok(())
    }

    pub fn set_inflow_options(
        &mut self,
        handle: MeshHandle,
        options: InflowOptions,
    ) -> Result<(), FluidError> {
        self.mesh_state
            .validate_handle(handle, Some(MeshRole::Inflow))?;
        options.validate()?;
        let ok = unsafe {
            super::manifold_fluids_world_set_inflow_options(
                self.native,
                handle.slot,
                options.velocity.as_ptr(),
                options.inherit_motion,
            )
        };
        super::native_result(ok, "setting inflow options")
    }

    pub fn set_collider_friction(
        &mut self,
        handle: MeshHandle,
        friction: f32,
    ) -> Result<(), FluidError> {
        self.mesh_state
            .validate_handle(handle, Some(MeshRole::Collider))?;
        if !friction.is_finite() || !(0.0..=1.0).contains(&friction) {
            return Err(FluidError::input(
                "collider friction must be finite and in 0..=1",
            ));
        }
        let ok = unsafe {
            super::manifold_fluids_world_set_collider_friction(self.native, handle.slot, friction)
        };
        super::native_result(ok, "setting collider friction")
    }

    pub fn remove_mesh(&mut self, handle: MeshHandle) -> Result<(), FluidError> {
        let slot = self.mesh_state.validate_handle(handle, None)?;
        let ok = unsafe { super::manifold_fluids_world_remove_mesh(self.native, handle.slot) };
        super::native_result(ok, "removing fluid mesh")?;
        self.mesh_state.release_after_remove(slot);
        Ok(())
    }

    pub fn set_boundary_collisions(&mut self, collisions: [bool; 6]) -> Result<(), FluidError> {
        let values = collisions.map(i32::from);
        let ok = unsafe {
            super::manifold_fluids_world_set_boundary_collisions(
                self.native,
                values.as_ptr(),
                values.len(),
            )
        };
        super::native_result(ok, "setting fluid boundary collisions")
    }
}

fn flatten_mesh(mesh: &TriangleMesh) -> Result<(Vec<f32>, Vec<u32>), FluidError> {
    let mut vertices = Vec::with_capacity(
        mesh.vertices
            .len()
            .checked_mul(3)
            .ok_or_else(|| FluidError::input("mesh vertex count overflow"))?,
    );
    for vertex in &mesh.vertices {
        vertices.extend_from_slice(vertex);
    }
    let mut triangles = Vec::with_capacity(
        mesh.triangles
            .len()
            .checked_mul(3)
            .ok_or_else(|| FluidError::input("mesh triangle count overflow"))?,
    );
    for triangle in &mesh.triangles {
        triangles.extend_from_slice(triangle);
    }
    Ok((vertices, triangles))
}

fn normalize_pose(pose: BodyPose) -> Result<[f32; 7], FluidError> {
    if pose.position.iter().any(|value| !value.is_finite()) {
        return Err(FluidError::input("mesh pose position must be finite"));
    }
    if pose.rotation.iter().any(|value| !value.is_finite()) {
        return Err(FluidError::input("mesh pose rotation must be finite"));
    }
    let scale = pose
        .rotation
        .iter()
        .fold(0.0_f64, |max, &value| max.max(f64::from(value.abs())));
    if scale == 0.0 {
        return Err(FluidError::input("mesh pose rotation must be nonzero"));
    }
    let norm = pose
        .rotation
        .iter()
        .map(|&value| {
            let scaled = f64::from(value) / scale;
            scaled * scaled
        })
        .sum::<f64>()
        .sqrt()
        * scale;
    if !norm.is_finite() || norm == 0.0 {
        return Err(FluidError::input("mesh pose rotation cannot be normalized"));
    }
    let mut result = [0.0; 7];
    result[..3].copy_from_slice(&pose.position);
    for (index, &value) in pose.rotation.iter().enumerate() {
        result[index + 3] = (f64::from(value) / norm) as f32;
    }
    Ok(result)
}

fn validate_transformed_mesh(mesh: &TriangleMesh, pose: [f32; 7]) -> Result<(), FluidError> {
    let q = [
        f64::from(pose[3]),
        f64::from(pose[4]),
        f64::from(pose[5]),
        f64::from(pose[6]),
    ];
    for vertex in &mesh.vertices {
        let [x, y, z] = [
            f64::from(vertex[0]),
            f64::from(vertex[1]),
            f64::from(vertex[2]),
        ];
        let tx = 2.0 * (q[1] * z - q[2] * y);
        let ty = 2.0 * (q[2] * x - q[0] * z);
        let tz = 2.0 * (q[0] * y - q[1] * x);
        let rx = x + q[3] * tx + q[1] * tz - q[2] * ty + f64::from(pose[0]);
        let ry = y + q[3] * ty + q[2] * tx - q[0] * tz + f64::from(pose[1]);
        let rz = z + q[3] * tz + q[0] * ty - q[1] * tx + f64::from(pose[2]);
        if [rx, ry, rz]
            .iter()
            .any(|value| !value.is_finite() || value.abs() > f64::from(f32::MAX))
        {
            return Err(FluidError::input(
                "mesh pose produces non-finite transformed vertices",
            ));
        }
    }
    Ok(())
}

pub fn validate_mesh(mesh: &TriangleMesh) -> Result<(), FluidError> {
    if mesh.vertices.len() < 4 || mesh.triangles.len() < 4 {
        return Err(FluidError::input("mesh must contain a closed volume"));
    }
    if mesh.vertices.len() > i32::MAX as usize || mesh.triangles.len() > i32::MAX as usize {
        return Err(FluidError::input("mesh is too large for the native solver"));
    }
    if mesh
        .vertices
        .iter()
        .any(|vertex| vertex.iter().any(|value| !value.is_finite()))
    {
        return Err(FluidError::input("mesh vertices must be finite"));
    }
    let mut edges: HashMap<(u32, u32), Vec<(usize, bool)>> = HashMap::new();
    let mut neighbours = vec![Vec::new(); mesh.triangles.len()];
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            let value = f64::from(vertex[axis]);
            minimum[axis] = minimum[axis].min(value);
            maximum[axis] = maximum[axis].max(value);
        }
    }
    let scale = (0..3)
        .map(|axis| maximum[axis] - minimum[axis])
        .fold(1.0_f64, f64::max);
    for (triangle_index, triangle) in mesh.triangles.iter().enumerate() {
        let [a, b, c] = *triangle;
        if [a, b, c]
            .iter()
            .any(|&index| index as usize >= mesh.vertices.len())
        {
            return Err(FluidError::input("mesh triangle index is out of range"));
        }
        if a == b || b == c || c == a {
            return Err(FluidError::input("mesh contains a degenerate triangle"));
        }
        let va = mesh.vertices[a as usize];
        let vb = mesh.vertices[b as usize];
        let vc = mesh.vertices[c as usize];
        let ab = [
            f64::from(vb[0]) - f64::from(va[0]),
            f64::from(vb[1]) - f64::from(va[1]),
            f64::from(vb[2]) - f64::from(va[2]),
        ];
        let ac = [
            f64::from(vc[0]) - f64::from(va[0]),
            f64::from(vc[1]) - f64::from(va[1]),
            f64::from(vc[2]) - f64::from(va[2]),
        ];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let area2 = cross.iter().map(|value| value * value).sum::<f64>();
        if !area2.is_finite() || area2 <= 1.0e-24 * scale.max(1.0).powi(4) {
            return Err(FluidError::input("mesh contains a zero-area triangle"));
        }
        for (from, to) in [(a, b), (b, c), (c, a)] {
            let key = (from.min(to), from.max(to));
            let forward = from < to;
            let edge = edges.entry(key).or_default();
            if edge
                .iter()
                .any(|&(_, other_forward)| other_forward == forward)
            {
                return Err(FluidError::input(
                    "mesh edge winding is inconsistent or non-manifold",
                ));
            }
            if edge.len() >= 2 {
                return Err(FluidError::input("mesh edge is non-manifold"));
            }
            edge.push((triangle_index, forward));
            if edge.len() == 2 {
                let other = edge[0].0;
                neighbours[triangle_index].push(other);
                neighbours[other].push(triangle_index);
            }
        }
    }
    if edges.values().any(|edge| edge.len() != 2) {
        return Err(FluidError::input("mesh is open or has non-manifold edges"));
    }
    let volume_epsilon = 1.0e-12 * scale.max(1.0).powi(3);
    let mut visited = vec![false; mesh.triangles.len()];
    for start in 0..mesh.triangles.len() {
        if visited[start] {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut volume = 0.0_f64;
        let [origin_a, _, _] = mesh.triangles[start];
        let origin = mesh.vertices[origin_a as usize].map(f64::from);
        while let Some(index) = stack.pop() {
            let [a, b, c] = mesh.triangles[index];
            let va = mesh.vertices[a as usize].map(f64::from);
            let vb = mesh.vertices[b as usize].map(f64::from);
            let vc = mesh.vertices[c as usize].map(f64::from);
            let a = [va[0] - origin[0], va[1] - origin[1], va[2] - origin[2]];
            let b = [vb[0] - origin[0], vb[1] - origin[1], vb[2] - origin[2]];
            let c = [vc[0] - origin[0], vc[1] - origin[1], vc[2] - origin[2]];
            volume += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0;
            for &neighbour in &neighbours[index] {
                if !visited[neighbour] {
                    visited[neighbour] = true;
                    stack.push(neighbour);
                }
            }
        }
        if !volume.is_finite() || volume <= volume_epsilon {
            return Err(FluidError::input(
                "mesh volume must be finite, nonzero, and outward oriented",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Seconds};

    fn cube_mesh(min: f32, max: f32) -> TriangleMesh {
        TriangleMesh {
            vertices: vec![
                [min, min, min],
                [max, min, min],
                [max, max, min],
                [min, max, min],
                [min, min, max],
                [max, min, max],
                [max, max, max],
                [min, max, max],
            ],
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [3, 7, 6],
                [3, 6, 2],
                [0, 4, 7],
                [0, 7, 3],
                [1, 2, 6],
                [1, 6, 5],
            ],
        }
    }

    fn non_box_mesh(min: f32, max: f32) -> TriangleMesh {
        let mut mesh = cube_mesh(min, max);
        mesh.vertices[6] = [max + 0.2, max, max - 0.15];
        mesh
    }

    fn pose(position: [f32; 3]) -> BodyPose {
        BodyPose {
            position,
            rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn test_world(cells: [u32; 3]) -> FluidWorld {
        let mut world = FluidWorld::new(Config {
            cells,
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world.set_gravity([0.0, 0.0, 0.0]).expect("zero gravity");
        world
    }

    #[test]
    fn scene_physics_mesh_validates_closed_outward_volume() {
        let mesh = cube_mesh(0.5, 1.5);
        validate_mesh(&mesh).expect("closed cube should validate");
        let mut inward = mesh;
        for triangle in &mut inward.triangles {
            triangle.swap(1, 2);
        }
        let error = validate_mesh(&inward).expect_err("inward cube should be rejected");
        assert!(error.to_string().contains("outward oriented"));
    }

    #[test]
    fn scene_physics_mesh_initial_fill_and_roles_produce_native_particles() {
        let mut world = FluidWorld::new(Config {
            cells: [12, 12, 12],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world.set_gravity([0.0, 0.0, 0.0]).expect("zero gravity");
        world
            .set_boundary_collisions([true, true, true, true, true, true])
            .expect("closed boundaries");
        let mesh = non_box_mesh(0.75, 1.75);
        world
            .add_fluid_mesh(&mesh, pose([0.0; 3]), [0.0; 3])
            .expect("initial fill");
        let inflow = world
            .add_mesh(&mesh, MeshRole::Inflow, pose([0.0; 3]))
            .expect("inflow");
        world
            .set_inflow_options(
                inflow,
                InflowOptions {
                    velocity: [0.0, 0.0, 0.0],
                    inherit_motion: 0.0,
                },
            )
            .expect("inflow options");
        let collider = world
            .add_mesh(&mesh, MeshRole::Collider, pose([2.0, 0.0, 0.0]))
            .expect("collider");
        world
            .set_collider_friction(collider, 0.25)
            .expect("collider friction");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("native step");
        assert!(
            stats.particles > 0,
            "initial mesh fill produced no particles"
        );
        world.remove_mesh(inflow).expect("remove inflow");
        world.remove_mesh(collider).expect("remove collider");
    }

    #[test]
    fn scene_physics_mesh_handles_reject_foreign_stale_and_wrong_roles() {
        let config = Config {
            cells: [8, 8, 8],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        };
        let mesh = cube_mesh(0.25, 0.75);
        let mut first = FluidWorld::new(config).expect("first world");
        let mut second = FluidWorld::new(config).expect("second world");
        let handle = first
            .add_mesh(&mesh, MeshRole::Outflow, pose([0.0; 3]))
            .expect("outflow");
        assert!(second.set_mesh_enabled(handle, false).is_err());
        assert!(
            first
                .set_inflow_options(handle, InflowOptions::default())
                .is_err()
        );
        first.remove_mesh(handle).expect("remove outflow");
        assert!(first.set_mesh_enabled(handle, false).is_err());
        let reused = first
            .add_mesh(&mesh, MeshRole::Outflow, pose([0.0; 3]))
            .expect("reused slot");
        assert_ne!(handle, reused);
        first.remove_mesh(reused).expect("remove reused outflow");
    }

    #[test]
    fn scene_physics_mesh_multiple_sources_enable_and_remove_independently() {
        fn marker_position(world: &mut FluidWorld) -> [f32; 3] {
            let mut position = [0.0; 3];
            let mut velocity = [0.0; 3];
            let ok = unsafe {
                super::super::manifold_fluids_world_marker_motion(
                    world.native,
                    position.as_mut_ptr(),
                    velocity.as_mut_ptr(),
                )
            };
            super::super::native_result(ok, "marker motion").expect("marker motion");
            position
        }

        let mut world = test_world([12, 12, 12]);
        let left = cube_mesh(0.5, 1.0);
        let right = cube_mesh(2.0, 2.5);
        let left_handle = world
            .add_mesh(&left, MeshRole::Inflow, pose([0.0; 3]))
            .expect("left inflow");
        let right_handle = world
            .add_mesh(&right, MeshRole::Inflow, pose([0.0; 3]))
            .expect("right inflow");
        world
            .set_inflow_options(left_handle, InflowOptions::default())
            .expect("left options");
        world
            .set_inflow_options(right_handle, InflowOptions::default())
            .expect("right options");
        world
            .set_mesh_enabled(left_handle, false)
            .expect("disable left inflow");
        let right_only = world.step(Seconds(1.0 / 60.0)).expect("right inflow");
        assert!(
            right_only.particles > 0,
            "right inflow produced no native particles"
        );
        assert!(
            marker_position(&mut world)[0] > 1.5,
            "right inflow particles were not spatially separated"
        );
        world
            .remove_mesh(right_handle)
            .expect("remove right inflow");
        let right_drain = world
            .add_mesh(&right, MeshRole::Outflow, pose([0.0; 3]))
            .expect("right drain");
        world
            .set_mesh_enabled(right_drain, true)
            .expect("enable right drain");
        world
            .set_mesh_enabled(left_handle, true)
            .expect("re-enable left inflow");
        let left_only = world
            .step(Seconds(1.0 / 60.0))
            .expect("left inflow after right removal");
        assert!(
            left_only.particles > 0,
            "left inflow did not produce native particles after independent removal"
        );
        assert!(
            marker_position(&mut world)[0] < 1.5,
            "right drain did not clear the removed source region"
        );
        world
            .set_mesh_enabled(left_handle, false)
            .expect("disable left inflow");
        let left_drain = world
            .add_mesh(&left, MeshRole::Outflow, pose([0.0; 3]))
            .expect("left drain");
        world
            .set_mesh_enabled(left_drain, true)
            .expect("enable left drain");
        let emptied = world.step(Seconds(1.0 / 60.0)).expect("drain left region");
        assert_eq!(
            emptied.particles, 0,
            "disabled left source left particles behind"
        );
        world.remove_mesh(right_drain).expect("remove right drain");
        world.remove_mesh(left_drain).expect("remove left drain");
        world
            .set_mesh_enabled(left_handle, true)
            .expect("re-enable left inflow");
        let returned = world
            .step(Seconds(1.0 / 60.0))
            .expect("left source returns");
        assert!(
            returned.particles > 0,
            "left source did not return after re-enable"
        );
        assert!(
            marker_position(&mut world)[0] < 1.5,
            "re-enabled left source produced particles outside its region"
        );
        world.remove_mesh(left_handle).expect("remove left inflow");
    }

    #[test]
    fn scene_physics_mesh_outflow_removes_initial_particles() {
        fn run(with_outflow: bool) -> u32 {
            let mut world = test_world([12, 12, 12]);
            let fill = cube_mesh(1.0, 2.0);
            world
                .add_fluid_mesh(&fill, pose([0.0; 3]), [5.0, 0.0, 0.0])
                .expect("initial fill");
            if with_outflow {
                let drain = world
                    .add_mesh(&fill, MeshRole::Outflow, pose([0.0; 3]))
                    .expect("outflow");
                world.set_mesh_enabled(drain, true).expect("enable outflow");
            }
            world
                .step(Seconds(1.0 / 60.0))
                .expect("outflow step")
                .particles
        }

        let retained = run(false);
        let removed = run(true);
        assert!(retained > 0, "initial fill produced no native particles");
        assert!(
            removed < retained,
            "outflow did not remove particles: retained={retained}, removed={removed}"
        );
    }

    #[test]
    fn scene_physics_mesh_moving_rotated_collider_changes_native_motion() {
        fn run(with_collider: bool) -> (u32, [f32; 3], [f32; 3]) {
            let mut world = test_world([12, 12, 12]);
            world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
            let fill = cube_mesh(0.75, 1.75);
            world
                .add_fluid_mesh(&fill, pose([0.0; 3]), [0.0; 3])
                .expect("initial fill");
            if with_collider {
                let collider = world
                    .add_mesh(&fill, MeshRole::Collider, pose([0.0, -1.0, 0.0]))
                    .expect("collider");
                world
                    .set_mesh_motion(
                        collider,
                        pose([0.0, -1.0, 0.0]),
                        BodyPose {
                            position: [0.0, -0.5, 0.0],
                            rotation: [0.0, 0.0, 0.38268343, 0.9238795],
                        },
                        pose([0.0, 0.0, 0.0]),
                    )
                    .expect("moving collider");
            }
            let mut stats = world.step(Seconds(1.0 / 60.0)).expect("collider step");
            for _ in 0..7 {
                stats = world.step(Seconds(1.0 / 60.0)).expect("collider tick");
            }
            let mut position = [0.0; 3];
            let mut velocity = [0.0; 3];
            let ok = unsafe {
                super::super::manifold_fluids_world_marker_motion(
                    world.native,
                    position.as_mut_ptr(),
                    velocity.as_mut_ptr(),
                )
            };
            super::super::native_result(ok, "marker motion").expect("marker motion");
            (stats.particles, position, velocity)
        }

        let control = run(false);
        let moved = run(true);
        let difference: f32 = control
            .1
            .iter()
            .zip(moved.1.iter())
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            + control
                .2
                .iter()
                .zip(moved.2.iter())
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>();
        assert!(
            control.0 != moved.0 || difference > 0.001,
            "moving collider did not change native output"
        );
    }

    #[test]
    fn scene_physics_mesh_rectangular_boundary_flags_change_native_output() {
        fn run(closed_positive_x: bool) -> u32 {
            let mut world = test_world([12, 8, 16]);
            world
                .set_boundary_collisions([true, closed_positive_x, true, true, true, true])
                .expect("rectangular boundary flags");
            // The +X outlet removes particles past x=2.125 (domain wall
            // inset plus its two-cell buffer). Seed beside that outlet while
            // keeping Y/Z away from the other walls, without excess box cells.
            let fill = cube_mesh(0.75, 1.25);
            world
                .add_fluid_mesh(&fill, pose([1.0, 0.0, 0.0]), [4.0, 0.0, 0.0])
                .expect("boundary fill");
            let mut particles = 0;
            for _ in 0..4 {
                particles = world
                    .step(Seconds(1.0 / 60.0))
                    .expect("boundary step")
                    .particles;
            }
            particles
        }

        let closed = run(true);
        let open = run(false);
        assert!(closed > 0, "closed rectangular domain lost all particles");
        assert!(
            open < closed,
            "open +X boundary did not remove particles: open={open}, closed={closed}"
        );
    }

    #[test]
    fn scene_physics_mesh_invalid_inputs_are_atomic() {
        let mut open = cube_mesh(0.5, 1.5);
        open.triangles.pop();
        assert!(validate_mesh(&open).is_err(), "open mesh was accepted");
        let mut world = test_world([8, 8, 8]);
        let mesh = cube_mesh(0.5, 1.0);
        assert!(
            world
                .add_mesh(&mesh, MeshRole::Inflow, pose([0.0; 3]))
                .is_ok()
        );
        assert!(
            world
                .add_mesh(
                    &mesh,
                    MeshRole::Collider,
                    BodyPose {
                        position: [f32::NAN, 0.0, 0.0],
                        rotation: [0.0, 0.0, 0.0, 1.0],
                    },
                )
                .is_err()
        );
        let handle = world
            .add_mesh(&mesh, MeshRole::Inflow, pose([1.0, 0.0, 0.0]))
            .expect("valid role after invalid pose");
        assert!(
            world
                .set_inflow_options(
                    handle,
                    InflowOptions {
                        velocity: [f32::NAN, 0.0, 0.0],
                        inherit_motion: 0.0,
                    },
                )
                .is_err()
        );
        world
            .set_inflow_options(handle, InflowOptions::default())
            .expect("valid options after invalid options");
    }
}
