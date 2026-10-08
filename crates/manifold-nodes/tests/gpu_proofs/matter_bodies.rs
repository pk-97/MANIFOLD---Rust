//! Matter bodies on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md` P2a): the
//! prescribed pose each substep, the collider projection in the grid update,
//! and the solid lattice, each against CPU-computed values.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_physics::sdf::signed_distance_lattice;
use manifold_node_engine::water::fluid::{TICK, domain_layout};
use manifold_node_engine::water::liquid::bodies::{LiquidBody, LiquidShape, body_pose_at, pack_distance_atlas};
use manifold_node_engine::water::liquid::fields::FieldLattice;
use manifold_node_engine::water::liquid::lattice::LiquidLattice;
use manifold_node_engine::water::matter::{MatterGridNode, MatterPoint, REACTION_WORDS, momentum_unit};
use manifold_node_engine::{exec::execution_plan::ExecutionPlan, exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId, state_store::StateStore, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};

use manifold_node_engine::scene::transform::Transform;

use crate::matter_scene::{MatterScene, SceneSettings};
use crate::matter_transfer::{HostArray, output_of, set};

/// One atom fed by host arrays, run for single frames.
pub(crate) struct Bench {
    pub graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    state: StateStore,
    hosts: Vec<(NodeInstanceId, ResourceId)>,
    pub node: NodeInstanceId,
}

impl Bench {
    /// `inputs` are host arrays wired into `type_id`'s ports, `outputs` the
    /// ports read back; `configure` sets params before the plan compiles.
    pub(crate) fn new(
        type_id: &str,
        inputs: Vec<(&'static str, HostArray)>,
        outputs: &[&str],
        configure: impl FnOnce(&mut Graph, NodeInstanceId),
    ) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let node = graph.add_node(registry.construct(type_id).expect(type_id));
        let mut host_nodes = Vec::new();
        for (port, host) in inputs {
            let host_node = graph.add_node(Box::new(host));
            graph.connect((host_node, "out"), (node, port)).unwrap_or_else(|e| panic!("{port}: {e:?}"));
            host_nodes.push(host_node);
        }
        configure(&mut graph, node);
        for port in outputs {
            graph.add_external_output(node, port).unwrap_or_else(|e| panic!("{port}: {e:?}"));
        }
        let plan = compile(&graph).expect("bench compiles");
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
        let hosts = host_nodes.iter().map(|&h| (h, output_of(&plan, h, "out"))).collect();
        Self { graph, plan, executor: Executor::new(Box::new(backend)), state: StateStore::new(), hosts, node }
    }

    /// Fill the `index`th host array.
    pub(crate) fn fill<T: bytemuck::Pod>(&self, index: usize, data: &[T]) {
        let backend = self.executor.backend();
        let buffer = backend.array_buffer(backend.slot_for(self.hosts[index].1).expect("bound")).expect("array");
        assert!(std::mem::size_of_val(data) as u64 <= buffer.size, "host array {index} too small");
        // SAFETY: shared storage, nothing in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
    }

    pub(crate) fn set(&mut self, name: &str, value: f32) {
        set(&mut self.graph, self.node, name, value);
    }

    pub(crate) fn run(&mut self) {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(TICK), frame_count: 0 };
        let mut enc = device.create_encoder("matter-bodies");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            self.executor.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
        }
        enc.commit_and_wait_completed();
    }

    pub(crate) fn read<T: bytemuck::Pod>(&self, port: &str) -> Vec<T> {
        let res = output_of(&self.plan, self.node, port);
        let buffer = self
            .executor
            .host_array_buffer(&self.graph, &self.plan, res)
            .expect("array holds its own contents");
        let ptr = buffer.mapped_ptr().expect("shared");
        let n = buffer.size as usize / std::mem::size_of::<T>();
        // SAFETY: the encoder completed; `n` whole elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), n).to_vec() }
    }
}

/// Two ticks of three bodies: one still, one sliding, one sliding and
/// turning about a tilted axis, one disabled. Every substep of the second tick
/// lands on the CPU pose (lerp translation, slerp rotation) within f32.
#[test]
fn matter_prescribed_pose_interpolates() {
    let tick = TICK as f32;
    let axis = [0.6f32, 0.0, 0.8];
    let turning = |rate: f32| [axis[0] * rate, axis[1] * rate, axis[2] * rate, 0.0];
    let row = |position: [f32; 3], velocity: [f32; 3], angular: [f32; 4], shape: f32| LiquidBody {
        position_inv_mass: [position[0], position[1], position[2], 0.0],
        rotation: [0.0, 0.3826834, 0.0, 0.9238795],
        linear_velocity: [velocity[0], velocity[1], velocity[2], 0.4],
        angular_velocity: angular,
        accel_shape: [0.0, 0.0, 0.0, shape],
        ..LiquidBody::default()
    };
    let rows = [
        // Tick 10 (first of the frame).
        row([0.0; 3], [0.0; 3], [0.0; 4], 0.0),
        row([1.0, 0.5, 0.0], [0.3, 0.0, 0.0], [0.0; 4], 1.0),
        row([-1.0, 0.2, 0.4], [0.0, -0.6, 0.1], turning(2.0), -1.0),
        // Tick 11.
        row([0.0; 3], [0.0; 3], [0.0; 4], 0.0),
        row([1.0 + 0.3 * tick, 0.5, 0.0], [0.3, 0.0, 0.0], [0.0; 4], 1.0),
        row([-1.0, 0.2 - 0.6 * tick, 0.4 + 0.1 * tick], [0.0, -0.9, 0.1], turning(-3.5), 0.0),
    ];
    let mut bench = Bench::new(
        "node.matter_move_bodies",
        vec![("bodies", HostArray::new::<LiquidBody>(rows.len() as u32))],
        &["bodies_out"],
        |_, _| {},
    );
    bench.fill(0, &rows);
    let substeps = 34;
    let step_dt = tick / substeps as f32;
    for (name, value) in [("tick_index", 11.0), ("first_tick", 10.0), ("step_dt", step_dt), ("body_count", 3.0), ("rows", 6.0)] {
        bench.set(name, value);
    }
    let mut worst = (0.0f32, 0.0f32);
    for substep in 0..substeps {
        bench.set("substep_in_tick", substep as f32);
        bench.run();
        let out: Vec<LiquidBody> = bench.read("bodies_out");
        for (b, source) in rows[3..].iter().enumerate() {
            let (position, rotation) = body_pose_at(source, (substep + 1) as f32 * step_dt);
            let got = out[b];
            for (got, want) in got.position_inv_mass.iter().zip(position) {
                worst.0 = worst.0.max((got - want).abs());
            }
            for (got, want) in got.rotation.iter().zip(rotation) {
                worst.1 = worst.1.max((got - want).abs());
            }
            assert_eq!(got.linear_velocity, source.linear_velocity, "velocity passes through");
            assert_eq!(got.accel_shape, source.accel_shape, "shape passes through");
        }
    }
    eprintln!("matter_prescribed_pose_interpolates: worst position {:e} m, rotation {:e}", worst.0, worst.1);
    assert!(worst.0 < 1e-6 && worst.1 < 1e-6, "{worst:?}");
    // At the tick's last substep the turning body has made the full slerp.
    let out: Vec<LiquidBody> = bench.read("bodies_out");
    let half = 0.5 * 3.5 * tick;
    let q = rows[5].rotation;
    let d = [-axis[0] * half.sin(), 0.0, -axis[2] * half.sin(), half.cos()];
    let end = [
        d[3] * q[0] + d[0] * q[3] + d[1] * q[2] - d[2] * q[1],
        d[3] * q[1] - d[0] * q[2] + d[1] * q[3] + d[2] * q[0],
        d[3] * q[2] + d[0] * q[1] - d[1] * q[0] + d[2] * q[3],
        d[3] * q[3] - d[0] * q[0] - d[1] * q[1] - d[2] * q[2],
    ];
    assert!((0..4).all(|c| (out[2].rotation[c] - end[c]).abs() < 1e-5), "{:?} vs {end:?}", out[2].rotation);
    // A row past `rows` is disabled.
    bench.set("rows", 4.0);
    bench.run();
    let out: Vec<LiquidBody> = bench.read("bodies_out");
    assert_eq!(out[0].accel_shape[3], 0.0);
    assert_eq!(out[1].accel_shape[3], -1.0);
}

/// An axis-aligned box mesh, outward wound.
pub(crate) fn box_mesh(half: [f32; 3]) -> manifold_physics::TriangleMesh {
    let v = |x: usize, y: usize, z: usize| {
        [[-half[0], half[0]][x], [-half[1], half[1]][y], [-half[2], half[2]][z]]
    };
    manifold_physics::TriangleMesh {
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
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let ut = cross(u, t);
    std::array::from_fn(|i| v[i] + q[3] * t[i] + ut[i])
}

/// Trilinear distance from the packed atlas, as `gu_lattice` reads it.
fn lattice_at(atlas: &[u32], shape: &LiquidShape, g: [f32; 3]) -> f32 {
    let dims = [shape.dims_x, shape.dims_y, shape.dims_z];
    let value = |node: [u32; 3]| {
        let index = shape.atlas_offset + node[0] + dims[0] * (node[1] + dims[1] * node[2]);
        let word = atlas[(index / 2) as usize];
        half::f16::from_bits((word >> (16 * (index & 1))) as u16).to_f32()
    };
    let c: [f32; 3] = std::array::from_fn(|a| g[a].clamp(0.0, (dims[a] - 1) as f32));
    let base: [u32; 3] = std::array::from_fn(|a| (c[a].floor() as u32).min(dims[a] - 2));
    let f: [f32; 3] = std::array::from_fn(|a| c[a] - base[a] as f32);
    let mut sum = 0.0;
    for corner in 0..8u32 {
        let o = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
        let w: f32 = (0..3).map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] }).product();
        sum += w * value([base[0] + o[0], base[1] + o[1], base[2] + o[2]]);
    }
    sum
}

/// The collider branch of the grid update on the GPU matches its CPU mirror
/// node for node: a box turned 30° about y, sliding and spinning, in a still
/// grid of uniform velocity (no gravity, walls or clamp). Nodes inside the box
/// moving into it leave with no velocity into it.
#[test]
fn matter_grid_update_projects_colliders() {
    let lat = LiquidLattice::from_layout(&domain_layout(None, 1.0, 16).expect("unit domain"));
    let dx = lat.cell_size();
    let dt = 1.0e-3f32;
    let unit = momentum_unit(dx, f64::from(dt));
    let lattice = signed_distance_lattice(&box_mesh([0.2, 0.15, 0.1]), 0.4 / 32.0, 0.025).expect("box lattice");
    let mut atlas = Vec::new();
    pack_distance_atlas(&lattice.values, &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [lattice.origin[0], lattice.origin[1], lattice.origin[2], lattice.spacing],
        dims_x: lattice.dims[0],
        dims_y: lattice.dims[1],
        dims_z: lattice.dims[2],
        atlas_offset: 0,
        scale_min: [1.2, 1.0, 1.5, 1.0],
    };
    let angle = std::f32::consts::FRAC_PI_6;
    let body = LiquidBody {
        position_inv_mass: [0.5, 0.45, 0.5, 0.0],
        rotation: [0.0, (0.5 * angle).sin(), 0.0, (0.5 * angle).cos()],
        linear_velocity: [0.5, 0.0, 0.0, 0.3],
        angular_velocity: [0.0, 1.0, 0.0, 0.0],
        ..LiquidBody::default()
    };
    // Every node holds 8 mass units moving at v0.
    let v0 = [-1.0f32, 0.3, 0.2];
    let m_raw = 8 * 65_536i32;
    let to_raw = |v: f32| (v * m_raw as f32 / (unit * (65_536.0 / 134_217_728.0))).round() as i32;
    let nodes = lat.node_count() as usize;
    let accum: Vec<i32> = (0..nodes).flat_map(|_| [to_raw(v0[0]), to_raw(v0[1]), to_raw(v0[2]), m_raw]).collect();
    let mut bench = Bench::new(
        "node.matter_grid_update",
        vec![
            ("accum", HostArray::new::<i32>(accum.len() as u32)),
            ("grid", HostArray::new::<MatterGridNode>(nodes as u32)),
            ("bodies", HostArray::new::<LiquidBody>(1)),
            ("shapes", HostArray::new::<LiquidShape>(1)),
            ("atlas", HostArray::new::<u32>(atlas.len() as u32)),
        ],
        &["grid_out"],
        |_, _| {},
    );
    bench.fill(0, &accum);
    bench.fill(2, &[body]);
    bench.fill(3, &[shape]);
    bench.fill(4, &atlas);
    for (name, value) in [
        ("nodes_x", lat.nodes()[0] as f32), ("nodes_y", lat.nodes()[1] as f32), ("nodes_z", lat.nodes()[2] as f32),
        ("cell_size", dx), ("step_dt", dt), ("gravity_x", 0.0), ("gravity", 0.0), ("gravity_z", 0.0),
        ("closed_faces", 0.0), ("momentum_unit", unit), ("lattice_min_x", lat.min()[0]),
        ("lattice_min_y", lat.min()[1]), ("lattice_min_z", lat.min()[2]), ("body_count", 1.0),
    ] {
        bench.set(name, value);
    }
    bench.run();
    let grid: Vec<MatterGridNode> = bench.read("grid_out");

    let n = lat.nodes();
    let (mut projected, mut worst) = (0, 0.0f32);
    for idx in 0..nodes {
        let coord = [idx as u32 % n[0], (idx as u32 / n[0]) % n[1], idx as u32 / (n[0] * n[1])];
        let x: [f32; 3] = std::array::from_fn(|a| lat.min()[a] + coord[a] as f32 * dx);
        let v_before: [f32; 3] = std::array::from_fn(|a| accum[idx * 4 + a] as f32 / m_raw as f32 * (unit * (65_536.0 / 134_217_728.0)));
        let mut v = v_before;
        let p = [body.position_inv_mass[0], body.position_inv_mass[1], body.position_inv_mass[2]];
        let q = body.rotation;
        let rel: [f32; 3] = std::array::from_fn(|a| x[a] - p[a]);
        let local = rotate([-q[0], -q[1], -q[2], q[3]], rel);
        let g: [f32; 3] = std::array::from_fn(|a| (local[a] / shape.scale_min[a] - shape.origin_spacing[a]) / shape.origin_spacing[3]);
        let dims = [shape.dims_x, shape.dims_y, shape.dims_z];
        if (0..3).all(|a| g[a] >= 0.0 && g[a] <= (dims[a] - 1) as f32) && lattice_at(&atlas, &shape, g) < 0.0 {
            let d = |a: usize, s: f32| {
                let mut h = g;
                h[a] += s;
                lattice_at(&atlas, &shape, h)
            };
            let grad: [f32; 3] = std::array::from_fn(|a| (d(a, 0.5) - d(a, -0.5)) / shape.scale_min[a]);
            let world = rotate(q, grad);
            let len = world.iter().map(|c| c * c).sum::<f32>().sqrt();
            let normal = world.map(|c| c / len);
            let w = [body.angular_velocity[0], body.angular_velocity[1], body.angular_velocity[2]];
            let spin = [w[1] * rel[2] - w[2] * rel[1], w[2] * rel[0] - w[0] * rel[2], w[0] * rel[1] - w[1] * rel[0]];
            let v_body: [f32; 3] = std::array::from_fn(|a| body.linear_velocity[a] + spin[a]);
            let v_rel: [f32; 3] = std::array::from_fn(|a| v[a] - v_body[a]);
            let v_n: f32 = (0..3).map(|a| v_rel[a] * normal[a]).sum();
            if v_n < 0.0 {
                let t: [f32; 3] = std::array::from_fn(|a| v_rel[a] - v_n * normal[a]);
                let t_len = t.iter().map(|c| c * c).sum::<f32>().sqrt();
                let keep = (t_len + v_n * body.linear_velocity[3]).max(0.0);
                v = std::array::from_fn(|a| v_body[a] + if t_len > 0.0 { t[a] * keep / t_len } else { 0.0 });
                projected += 1;
                let after: f32 = (0..3).map(|a| (grid[idx].velocity_mass[a] - v_body[a]) * normal[a]).sum();
                assert!(after > -1e-4, "node {idx} still moves into the box: {after}");
            }
        }
        for (got, want) in grid[idx].velocity_mass.iter().zip(v) {
            worst = worst.max((got - want).abs());
        }
    }
    eprintln!("matter_grid_update_projects_colliders: {projected} nodes projected, worst {worst:e} m/s");
    assert!(projected > 50, "the box projected few nodes: {projected}");
    assert!(worst < 1e-4, "GPU and CPU projections differ by {worst} m/s");
}

/// A force or impulse lattice over `lat` holding `f` at each field node.
fn field_values(lat: &LiquidLattice, field: &FieldLattice, f: impl Fn([f32; 3]) -> [f32; 3]) -> Vec<[f32; 4]> {
    let [nx, ny, _] = field.nodes().map(|n| n as usize);
    (0..field.node_count())
        .map(|i| {
            let coord = [i % nx, (i / nx) % ny, i / (nx * ny)];
            let v = f(std::array::from_fn(|a| lat.min()[a] + coord[a] as f32 * field.spacing()));
            [v[0], v[1], v[2], 0.0]
        })
        .collect()
}

fn set_field(bench: &mut Bench, field: &FieldLattice) {
    for (name, value) in [
        ("field_nodes_x", field.nodes()[0] as f32), ("field_nodes_y", field.nodes()[1] as f32),
        ("field_nodes_z", field.nodes()[2] as f32), ("field_spacing", field.spacing()),
    ] {
        bench.set(name, value);
    }
}

/// Seam P8 (Forces and impulses for GPU liquids): the grid update adds
/// dt·(g + forces(x)) every substep and impulses(x) once, on the impulse
/// tick's first substep, each read from the domain's coarse lattices exactly
/// as `FieldLattice::sample` reads them on the CPU.
#[test]
fn matter_grid_update_applies_field_lattices() {
    let lat = LiquidLattice::from_layout(&domain_layout(None, 1.0, 16).expect("unit domain"));
    let dx = lat.cell_size();
    let dt = 1.0e-3f32;
    let unit = momentum_unit(dx, f64::from(dt));
    let scale = unit * (65_536.0 / 134_217_728.0);
    let v0 = [0.4f32, -0.2, 0.1];
    let m_raw = 8 * 65_536i32;
    let to_raw = |v: f32| (v * m_raw as f32 / scale).round() as i32;
    let nodes = lat.node_count() as usize;
    let accum: Vec<i32> = (0..nodes).flat_map(|_| [to_raw(v0[0]), to_raw(v0[1]), to_raw(v0[2]), m_raw]).collect();
    let field = FieldLattice::of(&lat);
    // Not linear, so the trilinear weights matter. Two lattices, one per tick
    // from tick 7, so each tick must read its own.
    let tick_forces = [
        field_values(&lat, &field, |p| [30.0 * p[1], -20.0 + 40.0 * p[0] * p[2], 15.0 * p[0]]),
        field_values(&lat, &field, |p| [-12.0 * p[2], 25.0 * p[0] * p[1], 8.0 - 18.0 * p[1]]),
    ];
    let forces: Vec<[f32; 4]> = tick_forces.concat();
    let impulses = field_values(&lat, &field, |p| [0.5 - p[2], 2.0 * p[0] * p[1], 0.8 * p[1]]);
    let mut bench = Bench::new(
        "node.matter_grid_update",
        vec![
            ("accum", HostArray::new::<i32>(accum.len() as u32)),
            ("grid", HostArray::new::<MatterGridNode>(nodes as u32)),
            ("forces", HostArray::new::<f32>(8 * field.node_count() as u32)),
            ("impulses", HostArray::new::<f32>(4 * field.node_count() as u32)),
        ],
        &["grid_out"],
        |_, _| {},
    );
    bench.fill(0, &accum);
    bench.fill(2, &forces);
    bench.fill(3, &impulses);
    let gravity = [0.0f32, -9.81, 0.0];
    for (name, value) in [
        ("nodes_x", lat.nodes()[0] as f32), ("nodes_y", lat.nodes()[1] as f32), ("nodes_z", lat.nodes()[2] as f32),
        ("cell_size", dx), ("step_dt", dt), ("gravity_x", gravity[0]), ("gravity", gravity[1]),
        ("gravity_z", gravity[2]), ("closed_faces", 0.0), ("momentum_unit", unit), ("lattice_min_x", lat.min()[0]),
        ("lattice_min_y", lat.min()[1]), ("lattice_min_z", lat.min()[2]), ("impulse_tick", 7.0),
        ("first_tick", 7.0),
    ] {
        bench.set(name, value);
    }
    set_field(&mut bench, &field);
    let n = lat.nodes();
    // (force lattices, tick, substep, impulse applied): with one lattice every
    // tick reads it; with one per tick, tick 8 reads the second.
    for (lattices, tick, substep, impulse) in
        [(2, 7, 0, true), (2, 7, 1, false), (2, 8, 0, false), (1, 8, 0, false), (0, 7, 0, true), (0, 6, 0, false)]
    {
        bench.set("force_lattices", lattices as f32);
        bench.set("tick_index", tick as f32);
        bench.set("substep_in_tick", substep as f32);
        bench.run();
        let grid: Vec<MatterGridNode> = bench.read("grid_out");
        let mut worst = 0.0f32;
        for idx in 0..nodes {
            let coord = [idx as u32 % n[0], (idx as u32 / n[0]) % n[1], idx as u32 / (n[0] * n[1])];
            let x: [f32; 3] = std::array::from_fn(|a| lat.min()[a] + coord[a] as f32 * dx);
            let v_before: [f32; 3] = std::array::from_fn(|a| accum[idx * 4 + a] as f32 / m_raw as f32 * scale);
            let force = match lattices {
                0 => [0.0; 3],
                1 => field.sample(&tick_forces[0], x),
                _ => field.sample(&tick_forces[(tick - 7) as usize], x),
            };
            let kick = if impulse { field.sample(&impulses, x) } else { [0.0; 3] };
            for a in 0..3 {
                let want = v_before[a] + dt * (gravity[a] + force[a]) + kick[a];
                worst = worst.max((grid[idx].velocity_mass[a] - want).abs());
            }
        }
        eprintln!(
            "matter_grid_update_applies_field_lattices: force lattices {lattices}, tick {tick}, substep {substep}: worst {worst:e} m/s"
        );
        assert!(worst < 1e-5, "force lattices {lattices}, tick {tick}, substep {substep}: GPU differs from CPU by {worst} m/s");
    }
}

/// Inside the box, the outward normal at `x`, as `liquid_collider_project`
/// takes it from the atlas.
fn collider_normal(atlas: &[u32], shape: &LiquidShape, body: &LiquidBody, x: [f32; 3]) -> Option<[f32; 3]> {
    let p = [body.position_inv_mass[0], body.position_inv_mass[1], body.position_inv_mass[2]];
    let q = body.rotation;
    let rel: [f32; 3] = std::array::from_fn(|a| x[a] - p[a]);
    let local = rotate([-q[0], -q[1], -q[2], q[3]], rel);
    let g: [f32; 3] = std::array::from_fn(|a| (local[a] / shape.scale_min[a] - shape.origin_spacing[a]) / shape.origin_spacing[3]);
    let dims = [shape.dims_x, shape.dims_y, shape.dims_z];
    if !(0..3).all(|a| g[a] >= 0.0 && g[a] <= (dims[a] - 1) as f32) || lattice_at(atlas, shape, g) >= 0.0 {
        return None;
    }
    let d = |a: usize, s: f32| {
        let mut h = g;
        h[a] += s;
        lattice_at(atlas, shape, h)
    };
    let grad: [f32; 3] = std::array::from_fn(|a| (d(a, 0.5) - d(a, -0.5)) / shape.scale_min[a]);
    let world = rotate(q, grad);
    let len = world.iter().map(|c| c * c).sum::<f32>().sqrt();
    // A flat spot (the box's medial point) has no normal; the GPU skips it too.
    (len > 0.0).then(|| world.map(|c| c / len))
}

/// Seam P8 (Forces and impulses for GPU liquids): the body reaction counts
/// the velocity the grid update gave each node from the field lattices, so a
/// dynamic body feels what a force or an impulse pushed into it. A still,
/// frictionless box in a still grid: the field is the only velocity, and the
/// words match Σ inv_mass·m·(v − v_projected) computed on the CPU.
#[test]
fn matter_body_reaction_counts_field_velocity() {
    let lat = LiquidLattice::from_layout(&domain_layout(None, 1.0, 16).expect("unit domain"));
    let dx = lat.cell_size();
    let dt = 1.0e-3f32;
    let unit = momentum_unit(dx, f64::from(dt));
    let lattice = signed_distance_lattice(&box_mesh([0.2, 0.15, 0.1]), 0.4 / 32.0, 0.025).expect("box lattice");
    let mut atlas = Vec::new();
    pack_distance_atlas(&lattice.values, &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [lattice.origin[0], lattice.origin[1], lattice.origin[2], lattice.spacing],
        dims_x: lattice.dims[0],
        dims_y: lattice.dims[1],
        dims_z: lattice.dims[2],
        atlas_offset: 0,
        scale_min: [1.2, 1.0, 1.5, 1.0],
    };
    let angle = std::f32::consts::FRAC_PI_6;
    let centre = [0.0f32, 0.5, 0.0];
    let body = LiquidBody {
        position_inv_mass: [centre[0], centre[1], centre[2], 2.0],
        rotation: [0.0, (0.5 * angle).sin(), 0.0, (0.5 * angle).cos()],
        ..LiquidBody::default()
    };
    let mass = 0.01f32;
    let nodes = lat.node_count();
    let grid = vec![MatterGridNode { velocity_mass: [0.0, 0.0, 0.0, mass], velocity_before: [0.0; 4] }; nodes as usize];
    let field = FieldLattice::of(&lat);
    let inward = |k: f32| move |p: [f32; 3]| -> [f32; 3] { std::array::from_fn(|a| k * (centre[a] - p[a]) + 0.1 * k * p[(a + 1) % 3]) };
    let forces = field_values(&lat, &field, inward(1000.0));
    // Tick 3 of a frame from tick 2 reads the second lattice; the first is a decoy.
    let tick_forces: Vec<[f32; 4]> = [field_values(&lat, &field, inward(-700.0)), forces.clone()].concat();
    let impulses = field_values(&lat, &field, inward(2.0));
    let substeps = 4.0f32;
    let mut bench = Bench::new(
        "node.matter_body_reaction",
        vec![
            ("grid", HostArray::new::<MatterGridNode>(nodes)),
            ("bodies", HostArray::new::<LiquidBody>(1)),
            ("shapes", HostArray::new::<LiquidShape>(1)),
            ("atlas", HostArray::new::<u32>(atlas.len() as u32)),
            ("reaction", HostArray::new::<i32>(REACTION_WORDS)),
            ("forces", HostArray::new::<f32>(8 * field.node_count() as u32)),
            ("impulses", HostArray::new::<f32>(4 * field.node_count() as u32)),
        ],
        &["reaction_out"],
        |_, _| {},
    );
    bench.fill(0, &grid);
    bench.fill(1, &[body]);
    bench.fill(2, &[shape]);
    bench.fill(3, &atlas);
    bench.fill(5, &tick_forces);
    bench.fill(6, &impulses);
    for (name, value) in [
        ("nodes_x", lat.nodes()[0] as f32), ("nodes_y", lat.nodes()[1] as f32), ("nodes_z", lat.nodes()[2] as f32),
        ("cell_size", dx), ("step_dt", dt), ("gravity_x", 0.0), ("gravity", 0.0), ("gravity_z", 0.0),
        ("closed_faces", 0.0), ("momentum_unit", unit), ("lattice_min_x", lat.min()[0]),
        ("lattice_min_y", lat.min()[1]), ("lattice_min_z", lat.min()[2]), ("body_count", 1.0),
        ("dynamic_count", 1.0), ("substeps_per_tick", substeps), ("impulse_tick", 3.0), ("tick_index", 3.0),
        ("first_tick", 2.0),
    ] {
        bench.set(name, value);
    }
    set_field(&mut bench, &field);
    let counts = 16_777_216.0 / f64::from(unit);
    let inv_mass = f64::from(body.position_inv_mass[3]);
    let n = lat.nodes();
    // (case, forces on, substep): the impulse lands on substep 0 only.
    for (case, forces_on, substep) in [("still", false, 1.0f32), ("impulse", false, 0.0), ("force", true, 2.0)] {
        bench.set("force_lattices", if forces_on { 2.0 } else { 0.0 });
        bench.set("substep_in_tick", substep);
        bench.fill(4, &[0i32; REACTION_WORDS as usize]);
        bench.run();
        let words: Vec<i32> = bench.read("reaction_out");
        let (mut dv, mut dl, mut pushed) = ([0.0f64; 3], [0.0f64; 3], 0u32);
        for idx in 0..nodes {
            let coord = [idx % n[0], (idx / n[0]) % n[1], idx / (n[0] * n[1])];
            let x: [f32; 3] = std::array::from_fn(|a| lat.min()[a] + coord[a] as f32 * dx);
            let v: [f32; 3] = match case {
                "impulse" => field.sample(&impulses, x),
                "force" => field.sample(&forces, x).map(|f| dt * f),
                _ => [0.0; 3],
            };
            let Some(normal) = collider_normal(&atlas, &shape, &body, x) else { continue };
            let v_n: f32 = (0..3).map(|a| v[a] * normal[a]).sum();
            if v_n >= 0.0 {
                continue;
            }
            // A still frictionless body keeps the tangential part, so the
            // node loses v_n·n.
            pushed += 1;
            let impulse: [f64; 3] = std::array::from_fn(|a| f64::from(mass) * f64::from(v_n * normal[a]));
            let arm: [f64; 3] = std::array::from_fn(|a| f64::from(x[a] - centre[a]));
            let moment = [
                arm[1] * impulse[2] - arm[2] * impulse[1],
                arm[2] * impulse[0] - arm[0] * impulse[2],
                arm[0] * impulse[1] - arm[1] * impulse[0],
            ];
            for a in 0..3 {
                dv[a] += inv_mass * impulse[a] * counts;
                dl[a] += inv_mass * moment[a] / f64::from(dx) * counts;
            }
        }
        let weight = f64::from(substep / substeps);
        let expected: Vec<f64> = [dv, dv.map(|v| weight * v), dl, dl.map(|v| weight * v)].concat();
        let worst = expected.iter().zip(&words).map(|(e, &w)| (f64::from(w) - e).abs()).fold(0.0, f64::max);
        let size = dv.iter().map(|v| v * v).sum::<f64>().sqrt();
        eprintln!(
            "matter_body_reaction_counts_field_velocity {case}: {pushed} nodes pushed, |Σ Δv| {size:.0} counts, worst word {worst:.2} counts"
        );
        if case == "still" {
            assert!(words.iter().all(|&w| w == 0), "a still grid pushes nothing: {words:?}");
            continue;
        }
        assert!(pushed > 50, "{case}: the field pushed few nodes into the box: {pushed}");
        assert!(size > 1.0e4, "{case}: the field moved little momentum: {dv:?}");
        // Each add rounds by under one count, stochastically; the f32
        // products add well under one more per word.
        assert!(worst < f64::from(pushed) + 1.0, "{case}: reaction words {words:?} against {expected:?}");
    }
}

/// BUG-n97i (coupled MPM loses momentum at the collider push-out), D30: the
/// momentum node.grid_to_matter's push-out takes from each point lands in the
/// dynamic body's reaction words. A still grid at Liveliness 1 leaves each
/// point's own velocity, so the push-out is the only velocity change; the
/// words must equal Σ inv_mass·m·(v_in − v_out) and its turning moment about
/// the centre of mass, and the (s/n)-weighted copies, within the hashed
/// rounding (under one count per add). A prescribed body (inv_mass 0) and
/// dynamic_count 0 each write nothing.
#[test]
fn matter_push_out_reaction_matches_removed_momentum() {
    let lat = LiquidLattice::from_layout(&domain_layout(None, 1.0, 16).expect("unit domain"));
    let dx = lat.cell_size();
    let dt = 1.0e-3f32;
    let unit = momentum_unit(dx, f64::from(dt));
    let density = 1000.0f32;
    let lattice = signed_distance_lattice(&box_mesh([0.2, 0.15, 0.1]), 0.4 / 32.0, 0.025).expect("box lattice");
    let mut atlas = Vec::new();
    pack_distance_atlas(&lattice.values, &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [lattice.origin[0], lattice.origin[1], lattice.origin[2], lattice.spacing],
        dims_x: lattice.dims[0],
        dims_y: lattice.dims[1],
        dims_z: lattice.dims[2],
        atlas_offset: 0,
        scale_min: [1.2, 1.0, 1.5, 1.0],
    };
    let angle = std::f32::consts::FRAC_PI_6;
    let dynamic = LiquidBody {
        position_inv_mass: [0.0, 0.5, 0.0, 2.0],
        rotation: [0.0, (0.5 * angle).sin(), 0.0, (0.5 * angle).cos()],
        linear_velocity: [0.5, 0.0, 0.0, 0.3],
        angular_velocity: [0.0, 1.0, 0.0, 0.0],
        ..LiquidBody::default()
    };
    // A 12³ block of points through the box, each moving at v0. The unit
    // domain spans x and z in [−0.5, 0.5] and y in [0, 1].
    let v0 = [-1.0f32, 0.3, 0.2];
    let v_rest = dx * dx * dx / 8.0;
    let centre = [0.0f64, 0.5, 0.0];
    let points: Vec<MatterPoint> = (0..12u32 * 12 * 12)
        .map(|i| {
            let c = [i % 12, (i / 12) % 12, i / 144];
            let position: [f32; 3] = std::array::from_fn(|a| centre[a] as f32 - 0.33 + 0.06 * c[a] as f32);
            MatterPoint {
                position,
                id: i + 1,
                velocity: v0,
                volume_ratio: 1.0,
                affine_x: [0.0, 0.0, 0.0, 1.0],
                affine_y: [0.0, 0.0, 0.0, v_rest],
                affine_z: [0.0; 4],
            }
        })
        .collect();
    let nodes = lat.node_count();
    let (substep, substeps) = (3.0f32, 8.0f32);
    let mut bench = Bench::new(
        "node.grid_to_matter",
        vec![
            ("points", HostArray::new::<MatterPoint>(points.len() as u32)),
            ("grid", HostArray::new::<MatterGridNode>(nodes)),
            ("bodies", HostArray::new::<LiquidBody>(1)),
            ("shapes", HostArray::new::<LiquidShape>(1)),
            ("atlas", HostArray::new::<u32>(atlas.len() as u32)),
            ("reaction", HostArray::new::<i32>(REACTION_WORDS)),
        ],
        &["points_out", "reaction_out"],
        |_, _| {},
    );
    bench.fill(1, &vec![MatterGridNode::default(); nodes as usize]);
    bench.fill(3, &[shape]);
    bench.fill(4, &atlas);
    for (name, value) in [
        ("nodes_x", lat.nodes()[0] as f32), ("nodes_y", lat.nodes()[1] as f32), ("nodes_z", lat.nodes()[2] as f32),
        ("cell_size", dx), ("step_dt", dt), ("lattice_min_x", lat.min()[0]), ("lattice_min_y", lat.min()[1]),
        ("lattice_min_z", lat.min()[2]), ("liveliness", 1.0), ("active_count", points.len() as f32),
        ("body_count", 1.0), ("density", density), ("momentum_unit", unit), ("tick_index", 7.0),
        ("substep_in_tick", substep), ("substeps_per_tick", substeps), ("dynamic_count", 1.0),
    ] {
        bench.set(name, value);
    }
    let run = |bench: &mut Bench, body: LiquidBody| -> (Vec<MatterPoint>, Vec<i32>) {
        bench.fill(0, &points);
        bench.fill(2, &[body]);
        bench.fill(5, &[0i32; REACTION_WORDS as usize]);
        bench.run();
        (bench.read("points_out"), bench.read("reaction_out"))
    };

    let (moved, words) = run(&mut bench, dynamic);
    let counts = 16_777_216.0 / f64::from(unit);
    let inv_mass = f64::from(dynamic.position_inv_mass[3]);
    let (mut dv, mut dl, mut pushed) = ([0.0f64; 3], [0.0f64; 3], 0u32);
    for (before, after) in points.iter().zip(&moved) {
        assert_eq!(after.id, before.id, "no point leaves the lattice");
        let lost: [f64; 3] = std::array::from_fn(|a| f64::from(before.velocity[a]) - f64::from(after.velocity[a]));
        if lost == [0.0; 3] {
            continue;
        }
        pushed += 1;
        let impulse = lost.map(|l| f64::from(v_rest) * f64::from(density) * l);
        let arm: [f64; 3] = std::array::from_fn(|a| f64::from(after.position[a]) - centre[a]);
        let moment = [
            arm[1] * impulse[2] - arm[2] * impulse[1],
            arm[2] * impulse[0] - arm[0] * impulse[2],
            arm[0] * impulse[1] - arm[1] * impulse[0],
        ];
        for a in 0..3 {
            dv[a] += inv_mass * impulse[a] * counts;
            dl[a] += inv_mass * moment[a] / f64::from(dx) * counts;
        }
    }
    let weight = f64::from(substep / substeps);
    let expected: Vec<f64> = [dv, dv.map(|v| weight * v), dl, dl.map(|v| weight * v)].concat();
    let worst = expected.iter().zip(&words).map(|(e, &w)| (f64::from(w) - e).abs()).fold(0.0, f64::max);
    eprintln!(
        "matter_push_out_reaction_matches_removed_momentum: {pushed} points pushed out, Σ Δv {:?} counts, worst word {worst:.2} counts",
        dv.map(|v| v.round())
    );
    assert!(pushed > 50, "the box pushed out few points: {pushed}");
    assert!(dv.iter().map(|v| v * v).sum::<f64>().sqrt() > 1.0e5, "the push-out moved little momentum: {dv:?}");
    // Each add rounds by under one count, stochastically; f32 products add
    // well under one more per word.
    assert!(worst < f64::from(pushed) + 1.0, "reaction words {words:?} against {expected:?}");
    assert!(words[12..16].iter().all(|&w| w == 0), "padding untouched: {words:?}");

    let prescribed = LiquidBody { position_inv_mass: [0.0, 0.5, 0.0, 0.0], ..dynamic };
    let (unchanged, words) = run(&mut bench, prescribed);
    assert!(words.iter().all(|&w| w == 0), "a prescribed body takes no reaction: {words:?}");
    assert_eq!(bytemuck::cast_slice::<MatterPoint, u32>(&unchanged), bytemuck::cast_slice::<MatterPoint, u32>(&moved), "the push-out itself does not depend on the body's mass");
    bench.set("dynamic_count", 0.0);
    let (_, words) = run(&mut bench, dynamic);
    assert!(words.iter().all(|&w| w == 0), "dynamic_count 0 writes nothing: {words:?}");
}

/// Signed distance to a box of `size` centred at `centre`, turned `yaw`
/// about y (the Euler convention node.transform_3d and the roles use).
fn box_distance(p: [f32; 3], centre: [f32; 3], yaw: f32, size: [f32; 3]) -> f32 {
    let d = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
    let (s, c) = yaw.sin_cos();
    let local = [d[0] * c - d[2] * s, d[1], d[0] * s + d[2] * c];
    let q: [f32; 3] = std::array::from_fn(|i| local[i].abs() - 0.5 * size[i]);
    let outside = q.map(|v| v.max(0.0));
    (outside[0] * outside[0] + outside[1] * outside[1] + outside[2] * outside[2]).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)
}

/// Run frames until the domain has taken its colliders and ticked once.
fn until_ticking(scene: &mut MatterScene, mut pose: impl FnMut(&mut MatterScene, f32)) {
    for _ in 0..600 {
        let next = scene.simulation_time() + TICK as f32;
        pose(scene, next);
        scene.tick();
        if scene.simulation_time() > 0.0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the colliders never became ready");
}

/// D11: a box lowered into a still pool while spinning about y pushes the
/// liquid aside; no point ends a tick more than half a cell inside it.
#[test]
fn matter_collider_penetration_bounded() {
    let size = [0.4, 0.15, 0.2];
    let pose = |t: f32| ([0.0, 0.6 - 0.35 * (t / 1.0).min(1.0), 0.0], 1.5 * t);
    let transform = |t: f32| {
        let (pos, yaw) = pose(t);
        Transform { pos, rot_euler: [0.0, yaw, 0.0], scale: size, ..Transform::default() }
    };
    let mut scene = MatterScene::new(&SceneSettings {
        fill_height: 0.3,
        colliders: vec![transform(0.0)],
        ..SceneSettings::default()
    });
    let dx = scene.lattice().cell_size();
    until_ticking(&mut scene, |scene, t| scene.set_collider(0, transform(t)));
    let (mut worst, mut touching) = (f32::INFINITY, 0usize);
    for _ in 0..90 {
        let next = scene.simulation_time() + TICK as f32;
        scene.set_collider(0, transform(next));
        scene.tick();
        let now = scene.simulation_time();
        let (centre, yaw) = pose(now);
        let (mut tick_worst, mut deep, mut at) = (f32::INFINITY, 0, [0.0f32; 3]);
        for point in scene.points().iter().filter(|p| p.id != 0) {
            let phi = box_distance(point.position, centre, yaw, size);
            worst = worst.min(phi);
            touching += usize::from(phi < dx);
            deep += usize::from(phi < -0.5 * dx);
            if phi < tick_worst {
                tick_worst = phi;
                let d = [point.position[0] - centre[0], point.position[1] - centre[1], point.position[2] - centre[2]];
                let (s, c) = yaw.sin_cos();
                at = [d[0] * c - d[2] * s, d[1], d[0] * s + d[2] * c];
            }
        }
        if std::env::var_os("MATTER_PENETRATION_TRACE").is_some() {
            eprintln!("  t {now:.3}: deepest {tick_worst:.4} at local {at:.3?}, {deep} points deeper than dx/2");
        }
        assert_eq!(scene.stats().nonfinite, 0);
    }
    eprintln!(
        "matter_collider_penetration_bounded: deepest point {worst:.5} m (bound {:.5}), {touching} point-ticks within a cell of the box",
        -0.5 * dx
    );
    assert!(touching > 1000, "the box never reached the liquid: {touching}");
    assert!(worst >= -0.5 * dx, "a point sits {worst} m inside the box");
}

/// D11: the frame's solid lattice is the walls and the body together: at
/// every node, the smaller of the distance to the nearest closed wall and a
/// turned cube's signed distance (within the half-precision, trilinear
/// sampling of its lattice near the cube; exact elsewhere).
#[test]
fn matter_solid_lattice_matches_bodies() {
    let size = [0.25; 3];
    let (centre, yaw) = ([0.1, 0.55, -0.05], 0.4);
    let transform = Transform { pos: centre, rot_euler: [0.0, yaw, 0.0], scale: size, ..Transform::default() };
    let mut scene = MatterScene::new(&SceneSettings {
        fill_height: 0.2,
        colliders: vec![transform],
        ..SceneSettings::default()
    });
    until_ticking(&mut scene, |scene, _| scene.set_collider(0, transform));
    scene.tick();
    let lat = scene.lattice();
    let dx = lat.cell_size();
    let solid = scene.solid_b();
    let low: [f32; 3] = std::array::from_fn(|d| lat.min()[d] + 3.0 * dx);
    let high: [f32; 3] = std::array::from_fn(|d| low[d] + lat.cells()[d] as f32 * dx);
    let far = lat.nodes().iter().map(|&n| (n as f32 * dx).powi(2)).sum::<f32>().sqrt();
    let n = lat.nodes();
    let (mut near, mut worst_near, mut worst_far, mut worst_reach) = (0, 0.0f32, 0.0f32, 0.0f32);
    for (idx, &value) in solid.iter().enumerate().take(lat.node_count() as usize) {
        let coord = [idx as u32 % n[0], (idx as u32 / n[0]) % n[1], idx as u32 / (n[0] * n[1])];
        let x: [f32; 3] = std::array::from_fn(|d| lat.min()[d] + coord[d] as f32 * dx);
        // Inside the walls the nearest one; past them the Euclidean depth
        // into the solid, as the FLIP engine's inverted box measures it.
        let gap: [f32; 3] = std::array::from_fn(|d| (x[d] - low[d]).min(high[d] - x[d]));
        let depth = gap.iter().map(|g| g.min(0.0).powi(2)).sum::<f32>().sqrt();
        let walls = if depth > 0.0 { -depth } else { gap.iter().fold(far, |m, &g| m.min(g)) };
        let body = box_distance(x, centre, yaw, size);
        // The cube's lattice reaches two spacings (1/16 of its side) past it.
        // Trilinear sampling of a box's distance is off by up to 0.21 spacings
        // in a cell centred on an edge (corners at ±h/2 read 0.35h, 0.5h, 0.5h
        // and −0.5h, averaging 0.21h where the true distance is 0), plus the
        // half-precision rounding: 0.3 spacings bounds it.
        if body < 0.25 / 16.0 {
            near += 1;
            worst_near = worst_near.max((value - walls.min(body)).abs());
        } else if body > 0.25 {
            // Past its lattice the cube reads at least its true distance, so
            // where the walls are nearer the lattice is exactly theirs.
            if walls < body {
                worst_far = worst_far.max((value - walls).abs());
            } else {
                worst_reach = worst_reach.max((value - body).abs());
            }
        }
    }
    eprintln!("matter_solid_lattice_matches_bodies: {near} nodes at the cube within {worst_near:.5} m, walls within {worst_far:e} m, the cube past its lattice within {worst_reach:.5} m");
    assert!(near > 100, "few nodes near the cube: {near}");
    let spacing = 0.25 / 32.0;
    assert!(worst_near < 0.3 * spacing, "near the cube the lattice is off by {worst_near} m");
    assert!(worst_far < 1e-5, "away from it the walls are off by {worst_far} m");
    // Past the lattice the edge point c's distance plus the gap: the true
    // distance is at least the gap plus the two-spacing padding p, and d(c)
    // is at most √3·p, so the excess is under (√3 − 1)·p, plus the sampling
    // bound at c.
    let padding = 2.0 * spacing;
    assert!(worst_reach < (3f32.sqrt() - 1.0) * padding + 0.3 * spacing, "past its lattice the cube is off by {worst_reach} m");
}

/// D11 with the fill (section 3.2): a box standing in the pool from the
/// start leaves its volume unseeded; the seeds it would have held are unused
/// slots (id 0) and every live seed starts outside it.
#[test]
fn matter_fill_skips_colliders() {
    let size = [0.3, 0.4, 0.25];
    let (centre, yaw) = ([0.05, 0.15, 0.0], 0.3);
    let transform = Transform { pos: centre, rot_euler: [0.0, yaw, 0.0], scale: size, ..Transform::default() };
    let mut scene = MatterScene::new(&SceneSettings {
        fill_height: 0.25,
        colliders: vec![transform],
        ..SceneSettings::default()
    });
    until_ticking(&mut scene, |scene, _| scene.set_collider(0, transform));
    let dx = scene.lattice().cell_size();
    let points = scene.points();
    let unused = points.iter().filter(|p| p.id == 0).count();
    let deepest = points
        .iter()
        .filter(|p| p.id != 0)
        .map(|p| box_distance(p.position, centre, yaw, size))
        .fold(f32::INFINITY, f32::min);
    // The box's submerged volume at 8 points per cell.
    let expected = (size[0] * size[2] * (0.25 - (centre[1] - 0.5 * size[1])) / (dx * dx * dx) * 8.0) as usize;
    eprintln!("matter_fill_skips_colliders: {unused} unused slots (about {expected} expected), deepest live point {deepest:.4} m after one tick");
    assert!(unused * 10 > expected * 8 && unused * 10 < expected * 12, "{unused} unused, {expected} expected");
    assert!(deepest > -0.5 * dx, "a live point starts {deepest} m inside the box");
}
