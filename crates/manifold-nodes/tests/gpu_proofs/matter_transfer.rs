//! MLS-MPM transfer atoms on the GPU: the grid carries exactly the points'
//! mass. The chain runs without a substep region: each frame is one substep
//! (clear → particle-to-grid → grid update → grid-to-particle), the point
//! buffer updated in place across frames. The GPU grid is pooled on world
//! tiles, read back as the dense lattice through the table particle-to-grid
//! built.

use std::borrow::Cow;

use manifold_core::fluid_domain::{FluidDomainLayout, domain_layout};
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::scene::world_tiles::{pool_len, table_words};
use manifold_node_engine::{
    exec::backend::Backend, exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext,
    exec::effect_node::EffectNodeType, exec::effect_node::FrameTime,
    exec::effect_node::NodeInstanceId, exec::execution::Executor,
    exec::execution_plan::ExecutionPlan, exec::execution_plan::ResourceId,
    exec::execution_plan::compile, exec::metal_backend::MetalBackend, graph::Graph,
    load::graph_loader::pre_allocate_resources, parameters::ParamDef, parameters::ParamType,
    parameters::ParamValue, persistence::PrimitiveRegistry, ports::ArrayType, ports::KnownItem,
    ports::NodeInput, ports::NodeOutput, ports::NodePort, ports::PortKind, ports::PortType,
    state_store::StateStore,
};
use manifold_water_gpu_mpm::matter::{
    MASS_SCALE, MatterGridNode, MatterPoint, SAND_DENSITY, field_min, mass_unit, matter_nodes,
    matter_pool_tiles, momentum_unit,
};
use manifold_water_liquid::bounds::{ALL_FACES, NO_KILL_HEIGHT};
use manifold_water_liquid::testkit::world_box::from_pool_under;

/// One substep's settings.
#[derive(Clone, Copy)]
struct Params {
    dt: f64,
    gravity: [f64; 3],
    density: f64,
    walls: u32,
    bounds: [[f64; 3]; 2],
}

/// An array the test fills after pre-allocation, sized by `max_capacity`.
pub(crate) struct HostArray {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl HostArray {
    pub(crate) fn new<T: KnownItem>(capacity: u32) -> Self {
        Self {
            type_id: EffectNodeType::new("test.host_array"),
            outputs: vec![NodePort {
                name: Cow::Borrowed("out"),
                ty: PortType::Array(ArrayType::of_known::<T>()),
                kind: PortKind::Output,
                required: false,
            }],
            params: vec![ParamDef {
                name: Cow::Borrowed("max_capacity"),
                label: "Capacity",
                ty: ParamType::Int,
                default: ParamValue::Float(capacity as f32),
                range: Some((0.0, 1.0e8)),
                enum_values: &[],
            }],
        }
    }
}

impl EffectNode for HostArray {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &self.params
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

pub(crate) struct Chain {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    state: StateStore,
    accum: ResourceId,
    tiles: ResourceId,
    p2g: NodeInstanceId,
    pool_tiles: u32,
    /// World node of dense node 0, and the dense grid's node counts.
    origin: [i32; 3],
    nodes: [u32; 3],
    frame: u32,
}

pub(crate) fn set(graph: &mut Graph, node: NodeInstanceId, name: &str, value: f32) {
    graph
        .set_param(node, name, ParamValue::Float(value))
        .unwrap_or_else(|e| panic!("set {name}: {e:?}"));
}

pub(crate) fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    plan.steps()
        .iter()
        .find(|s| s.node == node)
        .and_then(|s| s.outputs.iter().find(|(p, _)| *p == port).map(|&(_, r)| r))
        .unwrap_or_else(|| panic!("no output {port}"))
}

impl Chain {
    fn new(layout: &FluidDomainLayout, points: &[MatterPoint], p: &Params) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let (dx, min, nodes) = (
            layout.cell_size as f32,
            field_min(layout),
            matter_nodes(layout.cells),
        );
        let pool_tiles = matter_pool_tiles(nodes).unwrap();
        let pool = pool_len(pool_tiles) as u32;
        let mut graph = Graph::new();
        let pts = graph.add_node(Box::new(HostArray::new::<MatterPoint>(points.len() as u32)));
        let acc = graph.add_node(Box::new(HostArray::new::<i32>(4 * pool)));
        let grd = graph.add_node(Box::new(HostArray::new::<MatterGridNode>(pool)));
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        let p2g = add(&mut graph, "node.matter_to_grid");
        let solid = add(&mut graph, "node.matter_static_solid");
        let update = add(&mut graph, "node.matter_grid_update");
        let g2p = add(&mut graph, "node.grid_to_matter");
        graph.connect((pts, "out"), (p2g, "points")).unwrap();
        graph.connect((acc, "out"), (p2g, "accum")).unwrap();
        graph
            .connect((p2g, "accum_out"), (update, "accum"))
            .unwrap();
        graph.connect((grd, "out"), (update, "grid")).unwrap();
        graph.connect((pts, "out"), (g2p, "points")).unwrap();
        graph.connect((update, "grid_out"), (g2p, "grid")).unwrap();
        for port in ["tiles", "held", "fresh", "held_args", "built"] {
            graph.connect((p2g, port), (solid, port)).unwrap();
        }
        for port in ["held", "held_args"] {
            graph.connect((p2g, port), (update, port)).unwrap();
        }
        for node in [update, g2p] {
            graph.connect((p2g, "tiles"), (node, "tiles")).unwrap();
            graph
                .connect((solid, "solid"), (node, "static_solid"))
                .unwrap();
        }
        graph.add_external_output(p2g, "tiles").expect("tiles");

        for node in [p2g, g2p] {
            set(&mut graph, node, "active_count", points.len() as f32);
        }
        for node in [p2g, update, g2p, solid] {
            set(&mut graph, node, "cell_size", dx);
            set(&mut graph, node, "pool_tiles", pool_tiles as f32);
        }
        for node in [p2g, update, g2p] {
            set(&mut graph, node, "step_dt", p.dt as f32);
        }
        for node in [update, g2p] {
            for (axis, name) in ["x", "y", "z"].iter().enumerate() {
                set(
                    &mut graph,
                    node,
                    &format!("bounds_lo_{name}"),
                    p.bounds[0][axis] as f32,
                );
                set(
                    &mut graph,
                    node,
                    &format!("bounds_hi_{name}"),
                    p.bounds[1][axis] as f32,
                );
            }
        }
        let unit = momentum_unit(dx, p.dt);
        set(&mut graph, p2g, "momentum_unit", unit);
        set(&mut graph, update, "momentum_unit", unit);
        set(&mut graph, p2g, "density", p.density as f32);
        set(&mut graph, update, "gravity_x", p.gravity[0] as f32);
        set(&mut graph, update, "gravity", p.gravity[1] as f32);
        set(&mut graph, update, "gravity_z", p.gravity[2] as f32);
        set(&mut graph, update, "wall_faces", p.walls as f32);
        set(&mut graph, g2p, "open_faces", 0.0);
        set(&mut graph, g2p, "kill_height", NO_KILL_HEIGHT);
        set(&mut graph, g2p, "density", p.density as f32);

        let plan = compile(&graph).expect("matter chain compiles");
        let harness = manifold_node_engine::testkit::gpu_harness::shared();
        let device = &harness.device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
        let points_res = output_of(&plan, pts, "out");
        let accum = output_of(&plan, acc, "out");
        let fill = |resource: ResourceId, data: &[MatterPoint]| {
            let slot = backend.slot_for(resource).expect("points bound");
            let buffer = Backend::array_buffer(&backend, slot).expect("points buffer");
            // SAFETY: shared storage, nothing in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
        };
        fill(points_res, points);
        let tiles = output_of(&plan, p2g, "tiles");
        Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            state: StateStore::new(),
            accum,
            tiles,
            p2g,
            pool_tiles,
            origin: std::array::from_fn(|a| (min[a] / dx).round() as i32),
            nodes,
            frame: 0,
        }
    }

    /// One substep; frame k rounds with tick k, substep 0.
    fn step(&mut self) {
        set(&mut self.graph, self.p2g, "tick_index", self.frame as f32);
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(self.frame) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(self.frame),
        };
        let mut enc = device.create_encoder("matter-transfer");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            self.executor.execute_frame_with_state(
                &mut self.graph,
                &self.plan,
                time,
                &mut gpu,
                &mut self.state,
                0,
            );
        }
        enc.commit_and_wait().expect("GPU work completes");
        self.frame += 1;
    }

    fn read<T: bytemuck::Pod>(&self, res: ResourceId) -> Vec<T> {
        let buffer = self
            .executor
            .host_array_buffer(&self.graph, &self.plan, res)
            .expect("array holds its own contents");
        let ptr = buffer.mapped_ptr().expect("shared");
        let n = buffer.size as usize / std::mem::size_of::<T>();
        // SAFETY: the encoder completed; `n` whole elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), n).to_vec() }
    }

    /// The accumulator's four words a node over the dense lattice, x fastest.
    fn accum(&self) -> Vec<i32> {
        let pooled: Vec<[i32; 4]> = self.read(self.accum);
        let table: Vec<u32> = self.read(self.tiles);
        assert!(table.len() as u64 >= table_words(self.pool_tiles));
        from_pool_under(&pooled, &table, self.pool_tiles, self.origin, self.nodes)
            .into_iter()
            .flatten()
            .collect()
    }
}

fn layout() -> FluidDomainLayout {
    domain_layout(None, 1.0, 1.0 / 16.0).expect("16-cell unit domain")
}

/// 512 points (8 per cell over a 4×4×4-cell blob) resting on the floor,
/// with an affine velocity field, a non-zero C, a sheared F and a stress, so
/// every term of the transfer is exercised.
fn fixture(layout: &FluidDomainLayout) -> Vec<MatterPoint> {
    let dx = layout.cell_size as f32;
    let (min, nodes) = (field_min(layout), matter_nodes(layout.cells));
    let v0 = dx * dx * dx / 8.0;
    let floor = min[1] + 3.0 * dx;
    let centre_x = min[0] + 0.5 * (nodes[0] - 1) as f32 * dx;
    let centre_z = min[2] + 0.5 * (nodes[2] - 1) as f32 * dx;
    let mut out = Vec::with_capacity(512);
    for n in 0..512u32 {
        let (i, j, k) = (n % 8, (n / 8) % 8, n / 64);
        let jitter =
            |s: u32| ((n.wrapping_mul(2654435761).wrapping_add(s) >> 8) % 1000) as f32 / 1000.0;
        let position = [
            centre_x + (i as f32 - 4.0 + 0.25 + 0.5 * jitter(1)) * 0.5 * dx,
            floor + (j as f32 + 0.25 + 0.5 * jitter(2)) * 0.5 * dx,
            centre_z + (k as f32 - 4.0 + 0.25 + 0.5 * jitter(3)) * 0.5 * dx,
        ];
        let rel = [
            position[0] - centre_x,
            position[1] - floor,
            position[2] - centre_z,
        ];
        out.push(MatterPoint {
            position,
            id: n + 1,
            velocity: [0.3 + 0.5 * rel[1], -0.4 - 0.2 * rel[0], 0.1 * rel[2]],
            volume_ratio: 0.985 + 0.03 * jitter(4),
            affine_x: [0.1, 0.5, 0.0, 0.0],
            affine_y: [-0.2, 0.0, 0.0, v0],
            affine_z: [0.0, 0.0, 0.1, 0.0],
            f_x: [0.99, 0.02, 0.0, 0.0],
            f_y: [0.0, 0.98 + 0.03 * jitter(4), 0.0, 0.0],
            f_z: [0.0, 0.0, 1.0, 0.0],
            stress_diag: [-2.0e3, -3.0e3, -2.0e3, 0.0],
            stress_off: [500.0, 0.0, 0.0, 0.0],
        });
    }
    out
}

fn params() -> Params {
    Params {
        dt: 1.0e-3,
        gravity: [0.0, -9.81, 0.0],
        density: f64::from(SAND_DENSITY),
        walls: ALL_FACES,
        bounds: {
            let l = layout();
            [
                l.min.map(f64::from),
                std::array::from_fn(|a| f64::from(l.min[a] + l.size[a])),
            ]
        },
    }
}

#[test]
fn matter_grid_mass_matches_particle_mass() {
    let layout = layout();
    let points = fixture(&layout);
    let p = params();
    let mut chain = Chain::new(&layout, &points, &p);
    chain.step();
    let accum = chain.accum();
    let unit = f64::from(mass_unit(layout.cell_size as f32)) / f64::from(MASS_SCALE);
    let grid: f64 = accum.chunks_exact(4).map(|w| f64::from(w[3]) * unit).sum();
    let particles: f64 = points
        .iter()
        .map(|pt| f64::from(pt.affine_y[3]) * p.density)
        .sum();
    let rel = (grid - particles).abs() / particles;
    eprintln!("matter_grid_mass_matches_particle_mass: relative error {rel:.3e}");
    assert!(rel <= 1.0e-5, "grid {grid} vs particles {particles}");
}
