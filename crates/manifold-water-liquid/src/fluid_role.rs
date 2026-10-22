//! `FluidRole` — CPU payload carried on [`super::ports::PortType::FluidRole`] wires.
//!
//! A fluid role owns immutable prepared local-space geometry plus the live
//! authored controls consumed by the fluid runtime. Geometry remains an
//! `Arc` so graph execution can pass it through the CPU wire without cloning
//! mesh data.

use std::sync::{Arc, OnceLock};

use manifold_physics::{TriangleMesh, closed_triangles};
use manifold_physics::collision_shape::{water_lattice, water_plan};
use manifold_physics::sdf::DistanceLattice;

use manifold_node_engine::scene::transform::Transform;


/// Semantic role a prepared geometry source contributes to the fluid solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FluidRoleKind {
    InitialFill,
    Inflow,
    Outflow,
}

impl FluidRoleKind {
    /// Every kind by the index a role wire carries. Solids are never a role:
    /// the water reads the World's Colliders and Bodies
    /// (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision shapes)).
    pub const ALL: [Self; 3] = [Self::InitialFill, Self::Inflow, Self::Outflow];

    pub fn from_index(index: u32) -> Option<Self> {
        Self::ALL.get(index as usize).copied()
    }

    pub const fn index(self) -> u32 {
        self as u32
    }
}

/// The box a water's Initial Fill places, from its Initial Fill roles in
/// slot order, each `None` while switched off. A water takes one Initial
/// Fill; a second one switched on is refused by name.
pub fn initial_fill(fills: impl IntoIterator<Item = Option<Transform>>) -> Result<Option<Transform>, String> {
    let mut on = fills.into_iter().flatten();
    let fill = on.next();
    if on.next().is_some() {
        return Err("a water takes one Initial Fill; delete or switch off the others".into());
    }
    Ok(fill)
}

/// Takes the Initial Fill roles out of `roles`, since a liquid seeds them
/// rather than simulating them, and returns the box they place
/// ([`initial_fill`]).
pub fn take_initial_fill(roles: &mut [Option<FluidRole>]) -> Result<Option<Transform>, String> {
    let is_fill = |role: &Option<FluidRole>| role.as_ref().is_some_and(|role| role.kind == FluidRoleKind::InitialFill);
    let fill = initial_fill(roles.iter().filter(|role| is_fill(role)).flatten().map(|role| role.enabled.then_some(role.transform)));
    for role in roles.iter_mut().filter(|role| is_fill(role)) {
        *role = None;
    }
    fill
}

pub use manifold_physics::collision_shape::{DISTANCE_NODES_ALONG_LONGEST, LatticePlan, SCALE_STEPS_PER_OCTAVE, scale_steps};

/// Immutable prepared local-space geometry for a fluid role. Its lattice
/// comes from the collision-shape service (SCENE_DOCUMENT_DESIGN.md section
/// 7.8 (Physics and collision shapes)), shell thickness automatic.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFluidGeometry {
    pub meshes: Vec<TriangleMesh>,
    /// Derived from the meshes, never serialized.
    #[serde(skip)]
    open_parts: OnceLock<bool>,
    /// A static collider's exact mesh: it never moves in the simulation, so
    /// each water crops its lattice to its own domain. Set where the
    /// coupling builds it, never serialized.
    #[serde(skip)]
    fixed: bool,
}

impl PreparedFluidGeometry {
    pub fn new(meshes: Vec<TriangleMesh>) -> Self {
        Self { meshes, open_parts: OnceLock::new(), fixed: false }
    }

    /// A static collider's mesh, which each water crops to its domain.
    pub fn fixed(meshes: Vec<TriangleMesh>) -> Self {
        Self { fixed: true, ..Self::new(meshes) }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    /// Whether any mesh has an open part. Computed once; call it where the
    /// geometry is prepared so the content thread finds it cached.
    pub fn has_open_parts(&self) -> bool {
        *self.open_parts.get_or_init(|| {
            self.meshes
                .iter()
                .any(|mesh| closed_triangles(mesh).is_ok_and(|closed| closed.contains(&false)))
        })
    }

    /// The lattice a liquid with `cell_size` (world metres) builds for this
    /// geometry at role `scale`, cropped to `domain` (local min then max)
    /// when given ([`manifold_physics::collision_shape::water_plan`]).
    pub fn lattice_plan(&self, cell_size: f32, scale: [f32; 3], domain: Option<[[f32; 3]; 2]>) -> Result<LatticePlan, String> {
        water_plan(&self.meshes, self.has_open_parts(), cell_size, scale, domain)
            .map_err(|error| format!("Fluid role distance lattice: {error}"))
    }

    /// One lattice over the meshes to `plan`: closed parts solid, open parts
    /// shells. Costs nodes × triangles; never call it on the content thread.
    pub fn build_lattice(&self, plan: &LatticePlan) -> Result<Arc<DistanceLattice>, String> {
        water_lattice(&self.meshes, plan)
            .map(Arc::new)
            .map_err(|error| format!("Fluid role distance lattice: {error}"))
    }
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
    use manifold_physics::sdf::MAX_LATTICE_NODES;
    use manifold_node_engine::{exec::backend::Backend, exec::backend::MockBackend, exec::cpu_values::CpuWireWrites, bindings::NodeInputs, bindings::NodeOutputs, ports::PortType, exec::execution_plan::ResourceId};

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

    /// A liquid takes its Initial Fill out of its roles: one enabled fill is
    /// its box, a switched-off one is no fill, and two are refused by name.
    #[test]
    fn a_liquid_takes_one_initial_fill_out_of_its_roles() {
        let fill = |x: f32, enabled: bool| {
            Some(FluidRole { kind: FluidRoleKind::InitialFill, transform: Transform { pos: [x, 0.0, 0.0], ..Transform::default() }, enabled, ..role() })
        };
        let mut roles = vec![Some(role()), fill(1.0, true), None, fill(2.0, false)];
        assert_eq!(take_initial_fill(&mut roles).unwrap().map(|t| t.pos[0]), Some(1.0));
        assert!(roles[0].is_some(), "the inflow stays");
        assert!(roles[1..].iter().all(Option::is_none), "the bodies never see a fill");

        let mut none = vec![Some(role()), fill(2.0, false)];
        assert_eq!(take_initial_fill(&mut none), Ok(None), "a switched-off fill seeds nothing");

        let mut two = vec![fill(1.0, true), fill(2.0, true)];
        assert!(take_initial_fill(&mut two).unwrap_err().contains("one Initial Fill"));
    }

    #[test]
    fn scene_physics_fluid_role_cpu_wire_round_trip_preserves_arc_and_values() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::FluidRole, None, (0, 0));
        let bindings: &[(&'static str, manifold_node_engine::bindings::Slot)] = &[("role", slot)];
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

    /// One lattice spans every mesh of the role, spacing = longest extent / 32
    /// with two spacings of padding, whatever the liquid's cells. Outside the
    /// two halves it is the cube's exact distance; inside, each part's own
    /// distance (a bound on the union's depth, 0 on the faces where the halves
    /// touch) with the sign of the union.
    #[test]
    fn scene_physics_fluid_role_distance_lattice_unions_meshes() {
        let geometry = two_part_cube();
        assert!(!geometry.has_open_parts());
        let plan = geometry.lattice_plan(0.01, [3.0, 1.0, 1.0], None).unwrap();
        assert_eq!(plan, geometry.lattice_plan(0.5, [1.0; 3], None).unwrap(), "a closed solid ignores cells and scale");
        let lattice = geometry.build_lattice(&plan).unwrap();
        assert_eq!(lattice.spacing, 1.0 / DISTANCE_NODES_ALONG_LONGEST);
        assert_eq!(lattice.dims, [37; 3]);
        // Node 10 is x = −0.25, the middle of the left half.
        assert!((lattice.value([10, 18, 18]) + 0.25).abs() < 1e-5, "{}", lattice.value([10, 18, 18]));
        assert!(lattice.value([18, 18, 18]).abs() < 1e-5, "on the touching faces");
        assert!((lattice.value([0, 18, 18]) - 2.0 / 32.0).abs() < 1e-5);
        assert!((lattice.value([0, 0, 18]) - (2.0f32 * (2.0 / 32.0) * (2.0 / 32.0)).sqrt()).abs() < 1e-5);
    }

    fn ground(half: f32) -> TriangleMesh {
        TriangleMesh {
            vertices: vec![[-half, 0.0, -half], [half, 0.0, -half], [half, 0.0, half], [-half, 0.0, half]],
            triangles: vec![[0, 2, 1], [0, 3, 2]],
        }
    }

    /// Trilinear distance at a local point, as the solvers sample it.
    fn sample(lattice: &DistanceLattice, p: [f32; 3]) -> f32 {
        let g: [f32; 3] = std::array::from_fn(|axis| (p[axis] - lattice.origin[axis]) / lattice.spacing);
        let base: [u32; 3] = std::array::from_fn(|axis| g[axis].floor() as u32);
        let t: [f32; 3] = std::array::from_fn(|axis| g[axis] - base[axis] as f32);
        let mut value = 0.0;
        for corner in 0..8u32 {
            let offset: [u32; 3] = std::array::from_fn(|axis| (corner >> axis) & 1);
            let weight: f32 = (0..3).map(|axis| if offset[axis] == 1 { t[axis] } else { 1.0 - t[axis] }).product();
            value += weight * lattice.value(std::array::from_fn(|axis| base[axis] + offset[axis]));
        }
        value
    }

    /// A 20 m open ground plane in a 20 m liquid at the default resolution:
    /// the shell's top face, where the water rests, is within one water cell
    /// of the drawn floor. Before the lattice followed the liquid's cells its
    /// spacing was 20 / 32 m and the face sat two of those cells up.
    #[test]
    fn scene_physics_fluid_role_ground_plane_shell_follows_the_liquid_cells() {
        let layout = manifold_core::fluid_domain::domain_layout(None, 20.0, 64).unwrap();
        let cell = layout.cell_size as f32;
        let geometry = PreparedFluidGeometry::new(vec![ground(10.0)]);
        assert!(geometry.has_open_parts());
        let plan = geometry.lattice_plan(cell, [1.0; 3], None).unwrap();
        assert!(!plan.capped);
        assert!(plan.spacing <= cell, "{} > {cell}", plan.spacing);
        let lattice = geometry.build_lattice(&plan).unwrap();
        // March up from the floor to the first point outside the solid.
        let mut y = 0.0f32;
        while sample(&lattice, [0.3, y, -0.7]) < 0.0 {
            y += 0.001;
        }
        assert!(y > 0.0 && y <= cell + 1e-3, "the water rests {y} m above the floor; a cell is {cell} m");
        assert!(sample(&lattice, [0.3, -0.5 * y, -0.7]) < 0.0, "the shell holds below the floor too");
    }

    /// A plane too wide for the node cap at the liquid's cells coarsens until
    /// it fits and says so. Cropped to a 4 m water over it, the same plane
    /// keeps the cells: the lattice covers the domain and its padding, and
    /// the water rests on the drawn floor.
    #[test]
    fn scene_physics_fluid_role_lattice_cap_coarsens_and_a_domain_crop_keeps_the_cells() {
        let geometry = PreparedFluidGeometry::new(vec![ground(500.0)]);
        let plan = geometry.lattice_plan(0.05, [1.0; 3], None).unwrap();
        assert!(plan.capped && plan.spacing > 0.05);
        let nodes = manifold_physics::sdf::union_lattice_nodes(plan.extent, plan.spacing, 2.0 * plan.spacing, plan.shell_thickness, true);
        assert!(nodes <= MAX_LATTICE_NODES);

        let domain = [[-2.0, -1.0, -2.0], [2.0, 3.0, 2.0]];
        let cropped = geometry.lattice_plan(0.05, [1.0; 3], Some(domain)).unwrap();
        assert!(!cropped.capped && cropped.spacing <= 0.05, "{cropped:?}");
        let lattice = geometry.build_lattice(&cropped).unwrap();
        let reach = 2.0 * cropped.spacing + cropped.half_shell() + 1e-4;
        for axis in [0, 2] {
            let end = lattice.origin[axis] + (lattice.dims[axis] - 1) as f32 * lattice.spacing;
            // The far end rounds up to a whole node.
            assert!(lattice.origin[axis] >= domain[0][axis] - reach && end <= domain[1][axis] + reach + lattice.spacing, "axis {axis}: {lattice:?}");
        }
        assert!(sample(&lattice, [0.3, 0.0, -0.7]) < 0.0, "the floor holds");
        assert!(sample(&lattice, [0.3, 0.05 + cropped.spacing, -0.7]) > 0.0, "the water rests within a cell of the floor");
    }

    /// An open quad is a shell two water cells thick, and it is open again
    /// after a save.
    #[test]
    fn scene_physics_fluid_role_distance_lattice_open_mesh_is_a_shell() {
        let quad = TriangleMesh {
            vertices: vec![[-0.5, 0.0, -0.5], [0.5, 0.0, -0.5], [0.5, 0.0, 0.5], [-0.5, 0.0, 0.5]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let geometry = PreparedFluidGeometry::new(vec![quad]);
        let plan = geometry.lattice_plan(0.1, [1.0; 3], None).unwrap();
        assert_eq!(plan.shell_thickness, 0.2, "two water cells");
        let lattice = geometry.build_lattice(&plan).unwrap();
        let centre: [u32; 3] = std::array::from_fn(|axis| (-lattice.origin[axis] / lattice.spacing).round() as u32);
        let depth = -lattice.value(centre);
        assert!(depth > 0.0 && (depth - plan.half_shell()).abs() <= lattice.spacing, "{depth}");
        let restored: PreparedFluidGeometry =
            serde_json::from_slice(&serde_json::to_vec(&geometry).unwrap()).unwrap();
        assert!(restored.has_open_parts());
    }

    /// What is derived from the meshes is not serialized: the bytes are the
    /// same before and after it exists.
    #[test]
    fn scene_physics_fluid_role_derived_state_is_not_serialized() {
        let geometry = two_part_cube();
        let before = serde_json::to_vec(&geometry).unwrap();
        geometry.has_open_parts();
        assert_eq!(before, serde_json::to_vec(&geometry).unwrap());
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
                manifold_node_engine::exec::metal_backend::MetalBackend::without_device(
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
