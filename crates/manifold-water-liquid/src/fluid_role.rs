//! `FluidRole` — CPU payload carried on [`super::ports::PortType::FluidRole`] wires.
//!
//! A fluid role owns immutable prepared local-space geometry plus the live
//! authored controls consumed by the fluid runtime. Geometry remains an
//! `Arc` so graph execution can pass it through the CPU wire without cloning
//! mesh data.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use manifold_physics::mesh_bvh::MeshBvh;
use manifold_physics::{BodyHandle, TriangleMesh, closed_triangles};

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
pub fn initial_fill<T>(fills: impl IntoIterator<Item = Option<T>>) -> Result<Option<T>, String> {
    let mut on = fills.into_iter().flatten();
    let fill = on.next();
    if on.next().is_some() {
        return Err("a water takes one Initial Fill; delete or switch off the others".into());
    }
    Ok(fill)
}

/// A water's Initial Fill as its liquid seeds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InitialFill {
    /// The box the fill's sites span: a box fill's transform, or the world box
    /// round a shaped fill's mesh at its pose, axis-aligned.
    pub column: Transform,
    /// A shaped fill's slot in the roles, where it stays so the liquid builds
    /// its shape's distance lattice; None for a box.
    pub shape: Option<usize>,
}

/// A liquid's Initial Fills by the Water each seeds ([`FluidRole::water`]):
/// Waters that share one solver each bring their own.
pub type InitialFills = [Option<InitialFill>; crate::MAX_WATERS as usize];

/// Takes the Initial Fill roles out of `roles`, since a liquid seeds them
/// rather than simulating them, and returns the one that is on for each
/// Water ([`initial_fill`]: a Water takes one). A fill with a mesh is
/// shaped: it stays in `roles` and its column is the world box round the
/// mesh.
pub fn take_initial_fills(roles: &mut [Option<FluidRole>]) -> Result<InitialFills, String> {
    let is_fill = |role: &Option<FluidRole>| {
        role.as_ref()
            .is_some_and(|role| role.kind == FluidRoleKind::InitialFill)
    };
    let mut fills: InitialFills = [None; crate::MAX_WATERS as usize];
    for (water, fill) in fills.iter_mut().enumerate() {
        let slot = initial_fill(
            roles
                .iter()
                .enumerate()
                .filter(|(_, role)| {
                    is_fill(role) && role.as_ref().is_some_and(|role| role.water as usize == water)
                })
                .map(|(slot, role)| role.as_ref().filter(|role| role.enabled).map(|_| slot)),
        )?;
        let Some(slot) = slot else {
            continue;
        };
        let role = roles[slot].as_ref().expect("a fill's slot holds it");
        *fill = Some(if role.geometry.meshes.is_empty() {
            InitialFill {
                column: role.transform,
                shape: None,
            }
        } else {
            InitialFill {
                column: posed_column(role.geometry.bounds(), role.transform)
                    .ok_or("the Fill's mesh has no finite bounds")?,
                shape: Some(slot),
            }
        });
    }
    for (slot, role) in roles.iter_mut().enumerate() {
        if is_fill(role) && !fills.iter().flatten().any(|fill| fill.shape == Some(slot)) {
            *role = None;
        }
    }
    Ok(fills)
}

/// The axis-aligned box round the local box `bounds` (min then max) that
/// `transform` places, turned and scaled as the solver poses a role, as a
/// transform: its centre and size, unturned. None unless finite.
pub fn posed_column(bounds: [[f32; 3]; 2], transform: Transform) -> Option<Transform> {
    let [lo, hi] = bounds;
    let scale = transform.scale;
    let rotation = manifold_water_rigid::physics::pose_from_transform(transform).rotation;
    let centre = crate::bodies::rotate(
        rotation,
        std::array::from_fn(|i| 0.5 * (lo[i] + hi[i]) * scale[i]),
    );
    let [min, max] = crate::bodies::posed_box(
        std::array::from_fn(|i| transform.pos[i] + centre[i]),
        std::array::from_fn(|i| 0.5 * (hi[i] - lo[i]) * scale[i]),
        rotation,
    );
    let column = Transform {
        pos: std::array::from_fn(|i| 0.5 * (min[i] + max[i])),
        scale: std::array::from_fn(|i| max[i] - min[i]),
        ..Transform::default()
    };
    column
        .pos
        .iter()
        .chain(&column.scale)
        .all(|v| v.is_finite())
        .then_some(column)
}

pub use manifold_physics::collision_shape::scale_steps;

/// Immutable prepared local-space geometry for a fluid role. The water reads
/// it through a mesh tree (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and
/// collision shapes)).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFluidGeometry {
    pub meshes: Vec<TriangleMesh>,
    /// Derived from the meshes, never serialized.
    #[serde(skip)]
    open_parts: OnceLock<bool>,
    /// The meshes' local box, min then max. Derived, never serialized.
    #[serde(skip)]
    bounds: OnceLock<[[f32; 3]; 2]>,
    /// The meshes' tree, built once on a worker and shared by every solver
    /// that reads this solid. Derived, never serialized.
    #[serde(skip)]
    tree: OnceLock<TreeBuild>,
    #[serde(skip)]
    tree_started: AtomicBool,
    /// A static collider's exact mesh: it never moves in the simulation, so
    /// each water reads it through a tree instead of a lattice. Set where the
    /// coupling builds it, never serialized.
    #[serde(skip)]
    fixed: bool,
    /// The coupled Box3D body this solid was built for, so a reshaped body
    /// finds its own older lattice to stand in. Set where the coupling builds
    /// it, never serialized.
    #[serde(skip)]
    body: Option<BodyHandle>,
}

impl PreparedFluidGeometry {
    pub fn new(meshes: Vec<TriangleMesh>) -> Self {
        Self {
            meshes,
            open_parts: OnceLock::new(),
            bounds: OnceLock::new(),
            tree: OnceLock::new(),
            tree_started: AtomicBool::new(false),
            fixed: false,
            body: None,
        }
    }

    /// A static collider's mesh, which each water reads through a tree.
    pub fn fixed(meshes: Vec<TriangleMesh>) -> Self {
        Self {
            fixed: true,
            ..Self::new(meshes)
        }
    }

    /// This solid as coupled body `body`'s.
    pub fn of_body(self, body: BodyHandle) -> Self {
        Self {
            body: Some(body),
            ..self
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub fn body(&self) -> Option<BodyHandle> {
        self.body
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

    /// The meshes' local box, min then max (infinite without a vertex).
    /// Computed once.
    pub fn bounds(&self) -> [[f32; 3]; 2] {
        *self.bounds.get_or_init(|| {
            let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
            for vertex in self.meshes.iter().flat_map(|mesh| &mesh.vertices) {
                for i in 0..3 {
                    bounds[0][i] = bounds[0][i].min(vertex[i]);
                    bounds[1][i] = bounds[1][i].max(vertex[i]);
                }
            }
            bounds
        })
    }

    /// The meshes' BVH, which the GPU queries for distance and winding.
    /// Costs a BVH build; never call it on the content thread.
    pub fn build_tree(&self) -> Result<Arc<MeshBvh>, String> {
        MeshBvh::build(&self.meshes)
            .map(Arc::new)
            .map_err(|error| format!("Fluid role tree: {error}"))
    }

    /// The meshes' tree, built once on a worker whichever solver asks first;
    /// every Water, Sand and gas reading this solid then shares it. `None`
    /// while it builds; `wait` (offline) blocks for it, so no simulated time
    /// goes by while it builds.
    pub fn tree(self: &Arc<Self>, wait: bool) -> Result<Option<Arc<MeshBvh>>, String> {
        if let Some(built) = self.tree.get() {
            return built.clone().map(Some);
        }
        if !self.tree_started.swap(true, Ordering::AcqRel) {
            let source = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("fluid-role-tree".into())
                .spawn(move || {
                    // Always settle, so a waiter never waits on a build that panicked.
                    let built = std::panic::catch_unwind(|| source.build_tree())
                        .unwrap_or_else(|_| Err("Fluid role tree build panicked".into()));
                    let _ = source.tree.set(built);
                });
            if let Err(error) = spawned {
                let _ = self
                    .tree
                    .set(Err(format!("Fluid role tree could not start: {error}")));
            }
        }
        if wait {
            return self.tree.wait().clone().map(Some);
        }
        self.tree.get().cloned().transpose()
    }
}

type TreeBuild = Result<Arc<MeshBvh>, String>;

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
    /// The Water of the liquid solver this role seeds or feeds: its particles
    /// carry it ([`crate::birth_radius`]), below [`crate::MAX_WATERS`].
    pub water: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::{
        bindings::NodeInputs, bindings::NodeOutputs, exec::backend::Backend,
        exec::backend::MockBackend, exec::cpu_values::CpuWireWrites,
        exec::execution_plan::ResourceId, ports::PortType,
    };

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
            water: 0,
        }
    }

    /// A liquid takes its Initial Fills out of its roles: one enabled fill is
    /// its Water's box, a switched-off one is no fill, two of one Water are
    /// refused by name, and two Waters sharing the solver each keep theirs.
    #[test]
    fn a_liquid_takes_one_initial_fill_out_of_its_roles() {
        let fill = |x: f32, enabled: bool| {
            Some(FluidRole {
                geometry: Arc::new(PreparedFluidGeometry::default()),
                kind: FluidRoleKind::InitialFill,
                transform: Transform {
                    pos: [x, 0.0, 0.0],
                    ..Transform::default()
                },
                enabled,
                ..role()
            })
        };
        let mut roles = vec![Some(role()), fill(1.0, true), None, fill(2.0, false)];
        assert_eq!(
            take_initial_fills(&mut roles).unwrap()[0].map(|f| (f.column.pos[0], f.shape)),
            Some((1.0, None))
        );
        assert!(roles[0].is_some(), "the inflow stays");
        assert!(
            roles[1..].iter().all(Option::is_none),
            "the bodies never see a box fill"
        );

        let mut none = vec![Some(role()), fill(2.0, false)];
        assert_eq!(
            take_initial_fills(&mut none),
            Ok([None; crate::MAX_WATERS as usize]),
            "a switched-off fill seeds nothing"
        );

        let mut two = vec![fill(1.0, true), fill(2.0, true)];
        assert!(
            take_initial_fills(&mut two)
                .unwrap_err()
                .contains("one Initial Fill")
        );

        let of_water = |water: u32, x: f32| {
            fill(x, true).map(|role| FluidRole { water, ..role })
        };
        let mut shared = vec![of_water(3, 1.0), Some(role()), of_water(0, 2.0)];
        let fills = take_initial_fills(&mut shared).unwrap();
        let columns: Vec<_> = fills
            .iter()
            .enumerate()
            .filter_map(|(water, fill)| fill.map(|fill| (water, fill.column.pos[0])))
            .collect();
        assert_eq!(columns, [(0, 2.0), (3, 1.0)]);
    }

    /// A fill with a mesh stays among the roles, and its column is the world
    /// box round the mesh at its pose: a 1 x 2 x 1 block scaled 2 and turned
    /// 90 degrees about Z spans 4 along X and 2 along Y.
    #[test]
    fn a_shaped_fill_stays_and_spans_its_posed_mesh() {
        let block = TriangleMesh {
            vertices: vec![[0.0, 0.0, -0.5], [1.0, 2.0, 0.5], [0.0, 2.0, 0.5]],
            triangles: vec![[0, 1, 2]],
        };
        let shaped = FluidRole {
            geometry: Arc::new(PreparedFluidGeometry::new(vec![block])),
            kind: FluidRoleKind::InitialFill,
            transform: Transform {
                pos: [10.0, 0.0, 0.0],
                rot_euler: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
                scale: [2.0; 3],
                ..Transform::default()
            },
            ..role()
        };
        let mut roles = vec![Some(role()), Some(shaped)];
        let fill = take_initial_fills(&mut roles).unwrap()[0].expect("a fill");
        assert_eq!(fill.shape, Some(1));
        assert!(roles[1].is_some(), "the shaped fill stays for its lattice");
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-4);
        assert!(
            near(fill.column.scale, [4.0, 2.0, 2.0]),
            "{:?}",
            fill.column.scale
        );
        assert!(
            near(fill.column.pos, [8.0, 1.0, 0.0]),
            "{:?}",
            fill.column.pos
        );
        assert_eq!(
            fill.column.rot_euler, [0.0; 3],
            "the column is axis-aligned"
        );
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

    /// A closed box from `min` to `max`.
    fn cuboid(min: [f32; 3], max: [f32; 3]) -> TriangleMesh {
        let v = |x: usize, y: usize, z: usize| {
            [
                [min[0], max[0]][x],
                [min[1], max[1]][y],
                [min[2], max[2]][z],
            ]
        };
        TriangleMesh {
            vertices: vec![
                v(0, 0, 0),
                v(1, 0, 0),
                v(1, 1, 0),
                v(0, 1, 0),
                v(0, 0, 1),
                v(1, 0, 1),
                v(1, 1, 1),
                v(0, 1, 1),
            ],
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [2, 3, 7],
                [2, 7, 6],
                [1, 2, 6],
                [1, 6, 5],
                [0, 4, 7],
                [0, 7, 3],
            ],
        }
    }

    /// A unit cube split in two closed halves, as a two-mesh compound role.
    fn two_part_cube() -> PreparedFluidGeometry {
        PreparedFluidGeometry::new(vec![
            cuboid([-0.5; 3], [0.0, 0.5, 0.5]),
            cuboid([0.0, -0.5, -0.5], [0.5; 3]),
        ])
    }

    /// An open quad stays open after a save.
    #[test]
    fn scene_physics_fluid_role_open_mesh_stays_open_after_a_save() {
        let quad = TriangleMesh {
            vertices: vec![
                [-0.5, 0.0, -0.5],
                [0.5, 0.0, -0.5],
                [0.5, 0.0, 0.5],
                [-0.5, 0.0, 0.5],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let geometry = PreparedFluidGeometry::new(vec![quad]);
        assert!(geometry.has_open_parts());
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
