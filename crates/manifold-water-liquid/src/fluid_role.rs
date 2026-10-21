//! `FluidRole` — CPU payload carried on [`super::ports::PortType::FluidRole`] wires.
//!
//! A fluid role owns immutable prepared local-space geometry plus the live
//! authored controls consumed by the fluid runtime. Geometry remains an
//! `Arc` so graph execution can pass it through the CPU wire without cloning
//! mesh data.

use std::sync::{Arc, OnceLock};

use manifold_physics::{TriangleMesh, closed_triangles};
use manifold_physics::sdf::{DistanceLattice, MAX_LATTICE_NODES, signed_distance_union, union_lattice_nodes};

use manifold_node_engine::scene::transform::Transform;


/// Semantic role a prepared geometry source contributes to the fluid solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FluidRoleKind {
    InitialFill,
    Inflow,
    Outflow,
    Collider,
}

impl FluidRoleKind {
    /// Every kind in its Role menu order, the index a role wire carries.
    pub const ALL: [Self; 4] = [Self::InitialFill, Self::Inflow, Self::Outflow, Self::Collider];

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

/// Distance lattice nodes along the longest local axis of a role's geometry.
/// About 34³ nodes with padding: under a second of CPU for a 1,000-triangle
/// mesh on the worker (GPU_MPM_SOLVER_DESIGN.md D16).
pub const DISTANCE_NODES_ALONG_LONGEST: f32 = 32.0;

/// Steps per octave an open mesh's plan rounds its role's scale to, so a
/// scale moving within one step (about 9%) rebuilds nothing; the shell and
/// the spacing are off by at most half a step.
pub const SCALE_STEPS_PER_OCTAVE: f32 = 8.0;

/// A role's smallest and largest scale in eighth-octave steps, the only part
/// of the scale an open mesh's lattice plan reads.
pub fn scale_steps(scale: [f32; 3]) -> [i32; 2] {
    let step = |s: f32| (s.log2() * SCALE_STEPS_PER_OCTAVE).round() as i32;
    [step(scale[0].min(scale[1]).min(scale[2])), step(scale[0].max(scale[1]).max(scale[2]))]
}

/// Immutable prepared local-space geometry for a fluid role.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFluidGeometry {
    pub meshes: Vec<TriangleMesh>,
    /// World metres. How thick the distance lattice makes the meshes' open
    /// parts; closed parts ignore it (`signed_distance_union`).
    #[serde(default)]
    pub shell_thickness: f32,
    /// Derived from the meshes, never serialized.
    #[serde(skip)]
    open_parts: OnceLock<bool>,
}

/// The lattice a liquid builds for one role: body-local spacing and shell
/// thickness, both unscaled. Equal plans give equal lattices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatticePlan {
    pub spacing: f32,
    /// Local units: the role's world Shell Thickness, never under two of
    /// the liquid's cells, over its smallest scale.
    pub shell_thickness: f32,
    /// Whether any part is open, so the solid has shells.
    pub open: bool,
    /// Whether [`MAX_LATTICE_NODES`] forced a spacing coarser than the
    /// liquid's cells.
    pub capped: bool,
    /// The meshes' joint local bounds.
    pub extent: [f32; 3],
}

impl LatticePlan {
    /// Half the thickness a shell has on the lattice, local units: never
    /// under one spacing (`signed_distance_union`).
    pub fn half_shell(&self) -> f32 {
        (0.5 * self.shell_thickness).max(self.spacing)
    }
}

impl PreparedFluidGeometry {
    pub fn new(meshes: Vec<TriangleMesh>, shell_thickness: f32) -> Self {
        Self { meshes, shell_thickness, open_parts: OnceLock::new() }
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
    /// geometry at role `scale` (SCENE_SETUP_PROGRAMME.md D8 (open meshes are
    /// solids)). A closed solid's surface is exact at any spacing, so it
    /// keeps longest / 32 whatever samples it. A shell's faces sit at least
    /// one spacing from its surface, so with open parts the lattice is no
    /// coarser than the liquid's cells, and the shell is at least two cells
    /// thick so the liquid's grid sees it: a ground plane rests the water
    /// within a cell of where it is drawn at any size. If that needs more than
    /// [`MAX_LATTICE_NODES`], the spacing coarsens until it fits and the plan
    /// says so. The scale enters as [`scale_steps`], so one plan covers a
    /// scale moving within a step.
    pub fn lattice_plan(&self, cell_size: f32, scale: [f32; 3]) -> Result<LatticePlan, String> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for vertex in self.meshes.iter().flat_map(|mesh| &mesh.vertices) {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
        let extent: [f32; 3] = std::array::from_fn(|axis| max[axis] - min[axis]);
        let longest = extent.into_iter().fold(0.0f32, f32::max);
        if !(longest.is_finite() && longest > 0.0) {
            return Err("Fluid role geometry has no extent for a distance lattice".into());
        }
        let coarse = longest / DISTANCE_NODES_ALONG_LONGEST;
        let open = self.has_open_parts();
        if !open {
            return Ok(LatticePlan { spacing: coarse, shell_thickness: 0.0, open, capped: false, extent });
        }
        let smallest = scale[0].min(scale[1]).min(scale[2]);
        let largest = scale[0].max(scale[1]).max(scale[2]);
        if !(cell_size.is_finite() && cell_size > 0.0 && smallest > 0.0 && largest.is_finite()) {
            return Err("Fluid role distance lattice needs a positive cell size and scale".into());
        }
        let [smallest, largest] = scale_steps(scale).map(|step| (step as f32 / SCALE_STEPS_PER_OCTAVE).exp2());
        // A shell thinner than two of the liquid's cells is one its grid
        // cannot see, so that is the least it is.
        let shell_thickness = self.shell_thickness.max(2.0 * cell_size) / smallest;
        let wanted = coarse.min(cell_size / largest);
        let mut spacing = wanted;
        while union_lattice_nodes(extent, spacing, 2.0 * spacing, shell_thickness, true) > MAX_LATTICE_NODES {
            spacing *= 1.125;
        }
        Ok(LatticePlan { spacing, shell_thickness, open, capped: spacing > wanted, extent })
    }

    /// One lattice over every mesh to `plan`: closed parts solid, open parts
    /// shells. Costs nodes × triangles; never call it on the content thread.
    pub fn build_lattice(&self, plan: &LatticePlan) -> Result<Arc<DistanceLattice>, String> {
        signed_distance_union(&self.meshes, plan.spacing, 2.0 * plan.spacing, plan.shell_thickness)
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
        PreparedFluidGeometry::new(vec![cuboid(-0.5, 0.0), cuboid(0.0, 0.5)], 0.0)
    }

    fn lattice_at(geometry: &PreparedFluidGeometry, cell_size: f32, scale: [f32; 3]) -> Arc<DistanceLattice> {
        geometry.build_lattice(&geometry.lattice_plan(cell_size, scale).unwrap()).unwrap()
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
        let plan = geometry.lattice_plan(0.01, [3.0, 1.0, 1.0]).unwrap();
        assert_eq!(plan, geometry.lattice_plan(0.5, [1.0; 3]).unwrap(), "a closed solid ignores cells and scale");
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
        let geometry = PreparedFluidGeometry::new(vec![ground(10.0)], 0.05);
        assert!(geometry.has_open_parts());
        let plan = geometry.lattice_plan(cell, [1.0; 3]).unwrap();
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

    /// Shell Thickness is world metres: a role scaled 2× builds half the
    /// local thickness, so the world distance (local × smallest scale, how
    /// the solvers read it) inside the plane is half the authored thickness.
    #[test]
    fn scene_physics_fluid_role_shell_thickness_is_world_metres() {
        let geometry = PreparedFluidGeometry::new(vec![ground(0.5)], 0.5);
        for scale in [[1.0; 3], [2.0, 2.0, 2.0], [4.0, 2.0, 3.0]] {
            let plan = geometry.lattice_plan(0.05, scale).unwrap();
            let lattice = geometry.build_lattice(&plan).unwrap();
            let smallest = scale[0].min(scale[1]).min(scale[2]);
            let centre: [u32; 3] = std::array::from_fn(|axis| (-lattice.origin[axis] / lattice.spacing).round() as u32);
            assert!(
                (lattice.value(centre) * smallest + 0.25).abs() < 1e-5,
                "scale {scale:?}: {} world",
                lattice.value(centre) * smallest
            );
        }
    }

    /// A plane too wide for the node cap at the liquid's cells coarsens until
    /// it fits and says so.
    #[test]
    fn scene_physics_fluid_role_lattice_cap_coarsens_and_says_so() {
        let geometry = PreparedFluidGeometry::new(vec![ground(500.0)], 0.05);
        let plan = geometry.lattice_plan(0.05, [1.0; 3]).unwrap();
        assert!(plan.capped && plan.spacing > 0.05);
        let nodes = manifold_physics::sdf::union_lattice_nodes(plan.extent, plan.spacing, 2.0 * plan.spacing, plan.shell_thickness, true);
        assert!(nodes <= MAX_LATTICE_NODES);
    }

    /// An open quad is a shell of the geometry's thickness, and the thickness
    /// survives a save.
    #[test]
    fn scene_physics_fluid_role_distance_lattice_open_mesh_is_a_shell() {
        let quad = TriangleMesh {
            vertices: vec![[-0.5, 0.0, -0.5], [0.5, 0.0, -0.5], [0.5, 0.0, 0.5], [-0.5, 0.0, 0.5]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let geometry = PreparedFluidGeometry::new(vec![quad], 0.5);
        let lattice = lattice_at(&geometry, 0.1, [1.0; 3]);
        let centre: [u32; 3] = std::array::from_fn(|axis| (-lattice.origin[axis] / lattice.spacing).round() as u32);
        assert!((lattice.value(centre) + 0.25).abs() < 1e-5, "{}", lattice.value(centre));
        let restored: PreparedFluidGeometry =
            serde_json::from_slice(&serde_json::to_vec(&geometry).unwrap()).unwrap();
        assert_eq!(restored.shell_thickness, 0.5);
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
