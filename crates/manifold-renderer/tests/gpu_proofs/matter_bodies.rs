//! Matter bodies on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md` P2a): the
//! prescribed pose each substep, the collider projection in the grid update,
//! and the solid lattice, each against CPU-computed values.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_physics::sdf::signed_distance_lattice;
use manifold_renderer::node_graph::fluid::{TICK, domain_layout};
use manifold_renderer::node_graph::matter::{
    MatterBody, MatterGridNode, MatterLattice, MatterShape, body_pose_at, momentum_unit, pack_distance_atlas,
};
use manifold_renderer::node_graph::{
    ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId,
    PrimitiveRegistry, ResourceId, StateStore, compile, pre_allocate_resources,
};

use crate::harness;
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
        let device = &harness::shared().device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");
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
        let device = &harness::shared().device;
        let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(TICK), frame_count: 0 };
        let mut enc = device.create_encoder("matter-bodies");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            self.executor.execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
        }
        enc.commit_and_wait_completed();
    }

    pub(crate) fn read<T: bytemuck::Pod>(&self, port: &str) -> Vec<T> {
        let backend = self.executor.backend();
        let res = output_of(&self.plan, self.node, port);
        let buffer = backend.array_buffer(backend.slot_for(res).expect("bound")).expect("array");
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
    let row = |position: [f32; 3], velocity: [f32; 3], angular: [f32; 4], shape: f32| MatterBody {
        position_inv_mass: [position[0], position[1], position[2], 0.0],
        rotation: [0.0, 0.3826834, 0.0, 0.9238795],
        linear_velocity: [velocity[0], velocity[1], velocity[2], 0.4],
        angular_velocity: angular,
        accel_shape: [0.0, 0.0, 0.0, shape],
        ..MatterBody::default()
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
        vec![("bodies", HostArray::new::<MatterBody>(rows.len() as u32))],
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
        let out: Vec<MatterBody> = bench.read("bodies_out");
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
    let out: Vec<MatterBody> = bench.read("bodies_out");
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
    let out: Vec<MatterBody> = bench.read("bodies_out");
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
fn lattice_at(atlas: &[u32], shape: &MatterShape, g: [f32; 3]) -> f32 {
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
    let lat = MatterLattice::from_layout(&domain_layout(None, 1.0, 16).expect("unit domain"));
    let dx = lat.cell_size;
    let dt = 1.0e-3f32;
    let unit = momentum_unit(dx, f64::from(dt));
    let lattice = signed_distance_lattice(&box_mesh([0.2, 0.15, 0.1]), 0.4 / 32.0, 0.025).expect("box lattice");
    let mut atlas = Vec::new();
    pack_distance_atlas(&lattice.values, &mut atlas);
    let shape = MatterShape {
        origin_spacing: [lattice.origin[0], lattice.origin[1], lattice.origin[2], lattice.spacing],
        dims_x: lattice.dims[0],
        dims_y: lattice.dims[1],
        dims_z: lattice.dims[2],
        atlas_offset: 0,
        scale_min: [1.2, 1.0, 1.5, 1.0],
    };
    let angle = std::f32::consts::FRAC_PI_6;
    let body = MatterBody {
        position_inv_mass: [0.5, 0.45, 0.5, 0.0],
        rotation: [0.0, (0.5 * angle).sin(), 0.0, (0.5 * angle).cos()],
        linear_velocity: [0.5, 0.0, 0.0, 0.3],
        angular_velocity: [0.0, 1.0, 0.0, 0.0],
        ..MatterBody::default()
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
            ("bodies", HostArray::new::<MatterBody>(1)),
            ("shapes", HostArray::new::<MatterShape>(1)),
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
        ("nodes_x", lat.nodes[0] as f32), ("nodes_y", lat.nodes[1] as f32), ("nodes_z", lat.nodes[2] as f32),
        ("cell_size", dx), ("step_dt", dt), ("gravity_x", 0.0), ("gravity", 0.0), ("gravity_z", 0.0),
        ("closed_faces", 0.0), ("momentum_unit", unit), ("lattice_min_x", lat.min[0]),
        ("lattice_min_y", lat.min[1]), ("lattice_min_z", lat.min[2]), ("body_count", 1.0),
    ] {
        bench.set(name, value);
    }
    bench.run();
    let grid: Vec<MatterGridNode> = bench.read("grid_out");

    let n = lat.nodes;
    let (mut projected, mut worst) = (0, 0.0f32);
    for idx in 0..nodes {
        let coord = [idx as u32 % n[0], (idx as u32 / n[0]) % n[1], idx as u32 / (n[0] * n[1])];
        let x: [f32; 3] = std::array::from_fn(|a| lat.min[a] + coord[a] as f32 * dx);
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
