//! MLS-MPM transfer atoms on the GPU against the f64 CPU oracle
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 12 (Invariants and enforcement)).
//! The chain runs without a substep region: each frame is one substep
//! (clear → particle-to-grid → grid update → grid-to-particle), the point
//! buffer updated in place across frames.

use std::borrow::Cow;

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::fluid::domain_layout;
use manifold_renderer::node_graph::matter::reference::{self, Params, Point};
use manifold_renderer::node_graph::matter::{
    MASS_SCALE, MOMENTUM_SCALE, MatterGridNode, MatterLattice, MatterPoint, mass_unit, momentum_unit,
    rounding_hash, water_lambda,
};
use manifold_renderer::node_graph::{
    ArrayType, Backend, EffectNode, EffectNodeContext, EffectNodeType, ExecutionPlan, Executor,
    FrameTime, Graph, KnownItem, MetalBackend, NodeInput, NodeInstanceId, NodeOutput, NodePort,
    ParamDef, ParamType, ParamValue, PortKind, PortType, PrimitiveRegistry, ResourceId, StateStore,
    compile, pre_allocate_resources,
};

use crate::harness;

/// An array the test fills after pre-allocation, sized by `max_capacity`.
struct HostArray {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl HostArray {
    fn new<T: KnownItem>(capacity: u32) -> Self {
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
    fn depth_rule(&self) -> manifold_renderer::node_graph::depth_rule::DepthRule {
        manifold_renderer::node_graph::depth_rule::DepthRule::Terminal
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
    points: ResourceId,
    accum: ResourceId,
    p2g: NodeInstanceId,
    /// The sort's order and ranges, on the block path.
    sorted: Option<(ResourceId, ResourceId)>,
    frame: u32,
}

fn set(graph: &mut Graph, node: NodeInstanceId, name: &str, value: f32) {
    graph
        .set_param(node, name, ParamValue::Float(value))
        .unwrap_or_else(|e| panic!("set {name}: {e:?}"));
}

fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    plan.steps()
        .iter()
        .find(|s| s.node == node)
        .and_then(|s| s.outputs.iter().find(|(p, _)| *p == port).map(|&(_, r)| r))
        .unwrap_or_else(|| panic!("no output {port}"))
}

impl Chain {
    pub(crate) fn new(lat: &MatterLattice, points: &[MatterPoint], p: &Params) -> Self {
        Self::build(lat, points, p, None)
    }

    /// With `sort_source`, P2G takes the block path: the source points go
    /// through node.matter_to_particles and node.sort_particles_into_cells
    /// into one bin per stencil base cell, and P2G reads `order` and `ranges`.
    /// A source other than `points` leaves points outside their sorted cell.
    pub(crate) fn build(
        lat: &MatterLattice,
        points: &[MatterPoint],
        p: &Params,
        sort_source: Option<&[MatterPoint]>,
    ) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let nodes = lat.node_count();
        let mut graph = Graph::new();
        let pts = graph.add_node(Box::new(HostArray::new::<MatterPoint>(points.len() as u32)));
        let acc = graph.add_node(Box::new(HostArray::new::<i32>(4 * nodes)));
        let grd = graph.add_node(Box::new(HostArray::new::<MatterGridNode>(nodes)));
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        let zero = add(&mut graph, "node.zero_array");
        let p2g = add(&mut graph, "node.matter_to_grid");
        let update = add(&mut graph, "node.matter_grid_update");
        let g2p = add(&mut graph, "node.grid_to_matter");
        let src_sort = sort_source.map(|source| {
            let src = graph.add_node(Box::new(HostArray::new::<MatterPoint>(source.len() as u32)));
            let m2p = add(&mut graph, "node.matter_to_particles");
            let sort = add(&mut graph, "node.sort_particles_into_cells");
            graph.connect((src, "out"), (m2p, "points")).unwrap();
            graph.connect((m2p, "particles"), (sort, "particles")).unwrap();
            graph.connect((sort, "order"), (p2g, "order")).unwrap();
            graph.connect((sort, "cell_ranges"), (p2g, "ranges")).unwrap();
            let (centre, size) = lat.cell_sort_box();
            for (axis, name) in ["x", "y", "z"].iter().enumerate() {
                set(&mut graph, sort, &format!("center_{name}"), centre[axis]);
                set(&mut graph, sort, &format!("size_{name}"), size[axis]);
                set(&mut graph, p2g, &format!("blocks_{name}"), lat.blocks()[axis] as f32);
            }
            set(&mut graph, sort, "cell_size", lat.cell_size);
            (src, sort)
        });
        graph.connect((acc, "out"), (zero, "in")).unwrap();
        graph.connect((pts, "out"), (p2g, "points")).unwrap();
        graph.connect((zero, "out"), (p2g, "accum")).unwrap();
        graph.connect((p2g, "accum_out"), (update, "accum")).unwrap();
        graph.connect((grd, "out"), (update, "grid")).unwrap();
        graph.connect((pts, "out"), (g2p, "points")).unwrap();
        graph.connect((update, "grid_out"), (g2p, "grid")).unwrap();

        for node in [p2g, g2p] {
            set(&mut graph, node, "lattice_min_x", lat.min[0]);
            set(&mut graph, node, "lattice_min_y", lat.min[1]);
            set(&mut graph, node, "lattice_min_z", lat.min[2]);
            set(&mut graph, node, "active_count", points.len() as f32);
        }
        for node in [p2g, update, g2p] {
            set(&mut graph, node, "cell_size", lat.cell_size);
            set(&mut graph, node, "nodes_x", lat.nodes[0] as f32);
            set(&mut graph, node, "nodes_y", lat.nodes[1] as f32);
            set(&mut graph, node, "nodes_z", lat.nodes[2] as f32);
            set(&mut graph, node, "step_dt", p.dt as f32);
        }
        let unit = momentum_unit(lat.cell_size, p.dt);
        set(&mut graph, p2g, "momentum_unit", unit);
        set(&mut graph, update, "momentum_unit", unit);
        set(&mut graph, p2g, "lambda", p.lambda as f32);
        set(&mut graph, p2g, "cohesion", p.cohesion as f32);
        set(&mut graph, p2g, "density", p.density as f32);
        set(&mut graph, update, "gravity_x", p.gravity[0] as f32);
        set(&mut graph, update, "gravity", p.gravity[1] as f32);
        set(&mut graph, update, "gravity_z", p.gravity[2] as f32);
        let faces = p.closed.iter().enumerate().fold(0u32, |m, (i, &c)| m | (u32::from(c) << i));
        set(&mut graph, update, "closed_faces", faces as f32);
        set(&mut graph, g2p, "liveliness", p.liveliness as f32);
        set(&mut graph, g2p, "cohesion", p.cohesion as f32);

        let plan = compile(&graph).expect("matter chain compiles");
        let harness = harness::shared();
        let device = &harness.device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");
        let points_res = output_of(&plan, pts, "out");
        let accum = output_of(&plan, acc, "out");
        let fill = |resource: ResourceId, data: &[MatterPoint]| {
            let slot = backend.slot_for(resource).expect("points bound");
            let buffer = Backend::array_buffer(&backend, slot).expect("points buffer");
            // SAFETY: shared storage, nothing in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
        };
        fill(points_res, points);
        if let (Some((src, _)), Some(source)) = (src_sort, sort_source) {
            fill(output_of(&plan, src, "out"), source);
        }
        let sorted = src_sort.map(|(_, sort)| (output_of(&plan, sort, "order"), output_of(&plan, sort, "cell_ranges")));
        Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            state: StateStore::new(),
            points: points_res,
            accum,
            p2g,
            sorted,
            frame: 0,
        }
    }

    /// The sort's (order, ranges as start/count pairs), on the block path.
    fn sorted(&self) -> Option<(Vec<u32>, Vec<u32>)> {
        self.sorted.map(|(order, ranges)| (self.read(order), self.read(ranges)))
    }

    /// One substep; frame k rounds with tick k, substep 0 (the oracle's
    /// `Params { tick_index: k, .. }`).
    pub(crate) fn step(&mut self) {
        set(&mut self.graph, self.p2g, "tick_index", self.frame as f32);
        let device = &harness::shared().device;
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(self.frame) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(self.frame),
        };
        let mut enc = device.create_encoder("matter-transfer");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            self.executor
                .execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
        }
        enc.commit_and_wait_completed();
        self.frame += 1;
    }

    fn read<T: bytemuck::Pod>(&self, res: ResourceId) -> Vec<T> {
        let backend = self.executor.backend();
        let buffer = backend
            .array_buffer(backend.slot_for(res).expect("bound"))
            .expect("array");
        let ptr = buffer.mapped_ptr().expect("shared");
        let n = buffer.size as usize / std::mem::size_of::<T>();
        // SAFETY: the encoder completed; `n` whole elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), n).to_vec() }
    }

    pub(crate) fn points(&self) -> Vec<MatterPoint> {
        self.read(self.points)
    }

    fn accum(&self) -> Vec<i32> {
        self.read(self.accum)
    }
}

fn lattice() -> MatterLattice {
    MatterLattice::from_layout(&domain_layout(None, 1.0, 16).expect("16-cell unit domain"))
}

/// 512 points (8 per cell over a 4×4×4-cell blob) resting on the floor,
/// with an affine velocity field, a non-zero C and volume ratios either side
/// of 1, so every term of the transfer (stress, cohesion, walls, Liveliness)
/// is exercised.
fn fixture(lat: &MatterLattice) -> Vec<MatterPoint> {
    let dx = lat.cell_size;
    let v0 = dx * dx * dx / 8.0;
    let floor = lat.min[1] + 3.0 * dx;
    let centre_x = lat.min[0] + 0.5 * (lat.nodes[0] - 1) as f32 * dx;
    let centre_z = lat.min[2] + 0.5 * (lat.nodes[2] - 1) as f32 * dx;
    let mut out = Vec::with_capacity(512);
    for n in 0..512u32 {
        let (i, j, k) = (n % 8, (n / 8) % 8, n / 64);
        let jitter = |s: u32| ((n.wrapping_mul(2654435761).wrapping_add(s) >> 8) % 1000) as f32 / 1000.0;
        let position = [
            centre_x + (i as f32 - 4.0 + 0.25 + 0.5 * jitter(1)) * 0.5 * dx,
            floor + (j as f32 + 0.25 + 0.5 * jitter(2)) * 0.5 * dx,
            centre_z + (k as f32 - 4.0 + 0.25 + 0.5 * jitter(3)) * 0.5 * dx,
        ];
        let rel = [position[0] - centre_x, position[1] - floor, position[2] - centre_z];
        out.push(MatterPoint {
            position,
            id: n + 1,
            velocity: [0.3 + 0.5 * rel[1], -0.4 - 0.2 * rel[0], 0.1 * rel[2]],
            volume_ratio: 0.985 + 0.03 * jitter(4),
            affine_x: [0.1, 0.5, 0.0, 1.0],
            affine_y: [-0.2, 0.0, 0.0, v0],
            affine_z: [0.0, 0.0, 0.1, 0.0],
        });
    }
    out
}

fn params() -> Params {
    Params {
        dt: 1.0e-3,
        gravity: [0.0, -9.81, 0.0],
        lambda: water_lambda(1.0, 1.0),
        cohesion: 0.3,
        density: 1000.0,
        liveliness: 0.5,
        closed: [true; 6],
        fixed_point: false,
        tick_index: 0,
        substep_in_tick: 0,
    }
}

fn max_errors(gpu: &[MatterPoint], cpu: &[Point]) -> (f64, f64) {
    let mut dx = 0.0f64;
    let mut dv = 0.0f64;
    for (g, c) in gpu.iter().zip(cpu) {
        assert_eq!(g.id, c.id, "point {} alive-state differs", c.id);
        for d in 0..3 {
            dx = dx.max((f64::from(g.position[d]) - c.x[d]).abs());
            dv = dv.max((f64::from(g.velocity[d]) - c.v[d]).abs());
        }
    }
    (dx, dv)
}

#[test]
fn matter_transfer_matches_reference() {
    let lat = lattice();
    let points = fixture(&lat);
    let p = params();
    let mut chain = Chain::new(&lat, &points, &p);
    chain.step();
    let gpu = chain.points();
    let mut cpu: Vec<Point> = points.iter().map(Point::from).collect();
    reference::substep(&mut cpu, &lat, &p);
    let (dx, dv) = max_errors(&gpu, &cpu);
    eprintln!("matter_transfer_matches_reference: max |Δx| = {dx:.3e} m, max |Δv| = {dv:.3e} m/s");
    let mut fixed: Vec<Point> = points.iter().map(Point::from).collect();
    reference::substep(&mut fixed, &lat, &Params { fixed_point: true, ..p });
    let (fdx, fdv) = max_errors(&gpu, &fixed);
    eprintln!("  vs the fixed-point oracle: max |Δx| = {fdx:.3e} m, max |Δv| = {fdv:.3e} m/s");
    // Section 12's tolerances hold against the oracle that rounds each grid
    // contribution to Q = 2^20 exactly as the GPU does (D5).
    assert!(fdx <= 1.0e-5, "positions differ from the fixed-point oracle by {fdx:.3e} m");
    assert!(fdv <= 2.0e-5, "velocities differ from the fixed-point oracle by {fdv:.3e} m/s");
    // Against continuous f64 the positions still hold; velocities carry the
    // Q = 2^20 momentum quantization, largest at low-mass surface nodes
    // (1.05e-4 m/s measured at dt = 1 ms; one LSB is dx/dt/2^20 = 6e-5 m/s
    // per unit of node mass).
    assert!(dx <= 1.0e-5, "positions differ by {dx:.3e} m");
    assert!(dv <= 2.0e-4, "velocities differ by {dv:.3e} m/s");
}

#[test]
fn matter_grid_mass_matches_particle_mass() {
    let lat = lattice();
    let points = fixture(&lat);
    let p = params();
    let mut chain = Chain::new(&lat, &points, &p);
    chain.step();
    let accum = chain.accum();
    let unit = f64::from(mass_unit(lat.cell_size)) / f64::from(MASS_SCALE);
    let grid: f64 = accum.chunks_exact(4).map(|w| f64::from(w[3]) * unit).sum();
    let particles: f64 = points.iter().map(|pt| f64::from(pt.affine_y[3]) * p.density).sum();
    let rel = (grid - particles).abs() / particles;
    eprintln!("matter_grid_mass_matches_particle_mass: relative error {rel:.3e}");
    assert!(rel <= 1.0e-5, "grid {grid} vs particles {particles}");
}

/// The GPU rounds each accumulator word as the fixed-point oracle does (D5):
/// small words, where the rounding offset decides the result and f32 has
/// bits to spare, match exactly. A different hash on either side would miss
/// about half of them.
#[test]
fn matter_accumulator_words_match_fixed_point_oracle() {
    let lat = lattice();
    let points = fixture(&lat);
    let p = params();
    let mut chain = Chain::new(&lat, &points, &p);
    chain.step();
    let gpu = chain.accum();
    let mut fixed: Vec<Point> = points.iter().map(Point::from).collect();
    let grid = reference::substep(&mut fixed, &lat, &Params { fixed_point: true, ..p });
    let m_unit = f64::from(mass_unit(lat.cell_size));
    let to_mass = f64::from(MASS_SCALE) / m_unit;
    let to_momentum = f64::from(MOMENTUM_SCALE) / m_unit / f64::from(momentum_unit(lat.cell_size, p.dt));
    // Large words sum f32 contributions of up to 1e8 that partly cancel, so
    // they differ from the f64 oracle by f32 precision: tens of LSB, about
    // 1e-10·dx/dt of velocity at a full node.
    let (mut small, mut exact, mut worst_large) = (0usize, 0usize, 0.0f64);
    for (node, words) in gpu.chunks_exact(4).enumerate() {
        let oracle = [
            grid.momentum[node][0] * to_momentum,
            grid.momentum[node][1] * to_momentum,
            grid.momentum[node][2] * to_momentum,
            grid.mass[node] * to_mass,
        ];
        for (word, expected) in words.iter().zip(oracle) {
            let expected = expected.round();
            if expected.abs() < 4096.0 {
                small += 1;
                exact += usize::from(f64::from(*word) == expected);
            } else {
                worst_large = worst_large.max((f64::from(*word) - expected).abs());
            }
        }
    }
    let rate = exact as f64 / small as f64;
    eprintln!("matter_accumulator_words_match_fixed_point_oracle: {exact}/{small} small words exact ({rate:.4}), large words within {worst_large} LSB");
    assert!(small > 1000, "the fixture exercises few small words: {small}");
    assert!(rate >= 0.95, "small words match only {rate:.4}");
    assert!(worst_large <= 256.0, "large words differ by {worst_large} LSB");
}

/// D6: block-local P2G adds the same integers as one thread per point on
/// every path a point can take: summed with its sorted cell's points, added
/// node by node into its block's tile after leaving its cell, or added
/// globally after leaving the tile. The sort sources shift the points by
/// nothing, by 1.5 and 0.75 cells (every point leaves its cell), and by a
/// hashed amount within ±0.6 cells per axis (a mix of all three).
#[test]
fn matter_block_p2g_bit_identical() {
    let lat = lattice();
    let points = fixture(&lat);
    let p = params();
    // One step: the accumulator P2G wrote and the points G2P updated.
    let step = |source: Option<&[MatterPoint]>| {
        let mut chain = Chain::build(&lat, &points, &p, source);
        chain.step();
        let (ranked, covered) = chain.sorted().map_or((0, 0), |(order, ranges)| {
            (order.iter().filter(|&&i| i != u32::MAX).count(), ranges.chunks_exact(2).map(|r| r[1]).sum::<u32>())
        });
        (chain.accum(), chain.points(), ranked, covered)
    };
    let (plain, plain_points, _, _) = step(None);
    let dx = lat.cell_size;
    let base = |position: [f32; 3]| -> [i64; 3] {
        std::array::from_fn(|axis| ((position[axis] - lat.min[axis]) / dx - 0.5).floor() as i64)
    };
    // (summed in its cell, node by node in the tile, global) for a source.
    let paths = |source: &[MatterPoint]| {
        let mut tally = [0usize; 3];
        for (point, from) in points.iter().zip(source) {
            let (cell, now) = (base(from.position), base(point.position));
            let in_tile = (0..3).all(|axis| (0..=3).contains(&(now[axis] - cell[axis].div_euclid(4) * 4)));
            tally[if now == cell { 0 } else if in_tile { 1 } else { 2 }] += 1;
        }
        tally
    };
    let shift = |offset: &dyn Fn(u32, usize) -> f32| -> Vec<MatterPoint> {
        points
            .iter()
            .map(|pt| MatterPoint {
                position: std::array::from_fn(|axis| pt.position[axis] + offset(pt.id, axis) * dx),
                ..*pt
            })
            .collect()
    };
    let drifted = shift(&|_, axis| [1.5, 0.75, 0.0][axis]);
    let jittered = shift(&|id, axis| {
        (rounding_hash(id.wrapping_mul(3).wrapping_add(axis as u32)) >> 8) as f32 / 16_777_216.0 * 1.2 - 0.6
    });
    let nonzero = plain.iter().filter(|&&w| w != 0).count();
    eprintln!("matter_block_p2g_bit_identical: {nonzero} nonzero words over {} blocks", lat.blocks().iter().product::<u32>());
    assert!(nonzero > 1000, "the fixture touches few nodes: {nonzero}");
    for (name, source) in [("sorted", &points), ("drifted", &drifted), ("jittered", &jittered)] {
        let tally = paths(source);
        let (words, moved, ranked, covered) = step(Some(source));
        let differ = plain.iter().zip(&words).filter(|(a, b)| a != b).count();
        let points_differ = plain_points
            .iter()
            .zip(&moved)
            .filter(|(a, b)| bytemuck::bytes_of(*a) != bytemuck::bytes_of(*b))
            .count();
        eprintln!(
            "  {name}: sort ranked {ranked}, ranges cover {covered}; points summed in cell {}, node by node in tile {}, global {}; {differ} words and {points_differ} points differ",
            tally[0], tally[1], tally[2]
        );
        assert_eq!(covered as usize, points.len(), "{name}: the sort covered every point");
        assert_eq!(differ, 0, "{name}: block P2G differs from the per-point path");
        assert_eq!(points_differ, 0, "{name}: the points after the step differ from the per-point path");
        if name == "jittered" {
            assert!(tally.iter().all(|&n| n >= 20), "{name} exercises every path: {tally:?}");
        }
    }
}

#[test]
fn matter_hundred_substeps_match_reference() {
    let lat = lattice();
    let points = fixture(&lat);
    let p = params();
    let mut chain = Chain::new(&lat, &points, &p);
    let mut cpu: Vec<Point> = points.iter().map(Point::from).collect();
    let mut fixed = cpu.clone();
    for step in 0..100 {
        chain.step();
        reference::substep(&mut cpu, &lat, &p);
        reference::substep(&mut fixed, &lat, &Params { fixed_point: true, tick_index: step, ..p });
    }
    let (dx, dv) = max_errors(&chain.points(), &cpu);
    eprintln!("matter_hundred_substeps_match_reference: max |Δx| = {dx:.3e} m, max |Δv| = {dv:.3e} m/s");
    let (fdx, fdv) = max_errors(&chain.points(), &fixed);
    eprintln!("  vs the fixed-point oracle: max |Δx| = {fdx:.3e} m, max |Δv| = {fdv:.3e} m/s");
    // f32 against f64 over 100 substeps (4.7e-6 m, 8.4e-5 m/s measured).
    assert!(fdx <= 1.0e-5, "positions differ from the fixed-point oracle by {fdx:.3e} m");
    assert!(fdv <= 2.0e-4, "velocities differ from the fixed-point oracle by {fdv:.3e} m/s");
    // Plus the accumulated Q = 2^20 quantization (9.6e-5 m, 1.4e-3 m/s
    // measured): a fraction of a millimetre against a 62.5 mm cell.
    assert!(dx <= 2.0e-4, "positions differ by {dx:.3e} m");
    assert!(dv <= 3.0e-3, "velocities differ by {dv:.3e} m/s");
}
