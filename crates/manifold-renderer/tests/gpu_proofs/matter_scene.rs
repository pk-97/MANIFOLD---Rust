//! The whole Live Matter graph on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 3.2): domain → fill → state and its substep region → frame. The
//! harness drives one fixed tick per frame and reads the point state, the
//! per-tick stats and the published frames back.

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuFrameProfile, GpuTextureFormat, GpuTimestampSampler};
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::fluid::TICK;
use manifold_renderer::node_graph::fluid_particles::FluidParticle;
use manifold_renderer::node_graph::matter::{MatterPoint, MatterTickStats, STATS_WORDS};
use manifold_renderer::node_graph::{
    ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId,
    ParamValue, PrimitiveRegistry, ResourceId, StateStore, Transform, compile,
    pre_allocate_resources,
};

use crate::harness;

/// Scene settings, in the domain node's own terms.
#[derive(Clone, Debug)]
pub(crate) struct SceneSettings {
    pub domain_size: f32,
    pub resolution: u32,
    pub fill_height: f32,
    pub column: Option<Transform>,
    pub gravity: [f32; 3],
    pub stiffness: f32,
    pub cohesion: f32,
    pub liveliness: f32,
    pub seed: u32,
    pub points_per_cell_27: bool,
    pub closed: [bool; 6],
}

impl Default for SceneSettings {
    fn default() -> Self {
        Self {
            domain_size: 1.0,
            resolution: 32,
            fill_height: 0.25,
            column: None,
            gravity: [0.0, -9.81, 0.0],
            stiffness: 1.0,
            cohesion: 0.0,
            liveliness: 0.0,
            seed: 0,
            points_per_cell_27: false,
            closed: [true; 6],
        }
    }
}

pub(crate) struct MatterScene {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    state: StateStore,
    pub domain: NodeInstanceId,
    pub frame_node: NodeInstanceId,
    pub state_node: NodeInstanceId,
    points: ResourceId,
    stats: ResourceId,
    frame_b: ResourceId,
    frame_count: u32,
}

const LATTICE: [&str; 7] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z",
];

impl MatterScene {
    pub(crate) fn new(settings: &SceneSettings) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        let domain = add(&mut graph, "node.matter_domain");
        let fill = add(&mut graph, "node.matter_fill");
        let state = add(&mut graph, "node.matter_state");
        let zero = add(&mut graph, "node.zero_array");
        let p2g = add(&mut graph, "node.matter_to_grid");
        let update = add(&mut graph, "node.matter_grid_update");
        let g2p = add(&mut graph, "node.grid_to_matter");
        let stats = add(&mut graph, "node.matter_stats");
        let frame = add(&mut graph, "node.matter_frame");
        fn wire(graph: &mut Graph, from: (NodeInstanceId, &'static str), to: (NodeInstanceId, &'static str)) {
            graph.connect(from, to).unwrap_or_else(|e| panic!("{from:?} -> {to:?}: {e:?}"));
        }
        for port in LATTICE {
            for node in [fill, p2g, g2p, frame] {
                wire(&mut graph, (domain, port), (node, port));
            }
            if port != "cell_size" {
                wire(&mut graph, (domain, port), (stats, port));
            }
        }
        for port in ["nodes_x", "nodes_y", "nodes_z", "cell_size"] {
            wire(&mut graph, (domain, port), (update, port));
        }
        for port in ["nodes_x", "nodes_y", "nodes_z"] {
            wire(&mut graph, (domain, port), (state, port));
        }
        for node in [update, frame] {
            wire(&mut graph, (domain, "closed_faces"), (node, "closed_faces"));
        }
        for port in ["gravity_x", "gravity", "gravity_z"] {
            wire(&mut graph, (domain, port), (update, port));
            wire(&mut graph, (domain, port), (stats, port));
        }
        for port in [
            "pool_cells", "column_x0", "column_x1", "column_y0", "column_y1", "column_z0",
            "column_z1", "points_per_cell",
        ] {
            wire(&mut graph, (domain, port), (fill, port));
        }
        wire(&mut graph, (domain, "fill_seed"), (fill, "seed"));
        for port in ["ticks", "substeps_per_tick", "epoch"] {
            wire(&mut graph, (domain, port), (state, port));
        }
        for port in ["simulation_time", "display_time", "epoch"] {
            wire(&mut graph, (domain, port), (frame, port));
        }
        for port in ["lambda", "cohesion", "density"] {
            wire(&mut graph, (domain, port), (p2g, port));
            wire(&mut graph, (domain, port), (stats, port));
        }
        wire(&mut graph, (domain, "liveliness"), (g2p, "liveliness"));

        wire(&mut graph, (fill, "points"), (state, "seed"));
        for (node, port) in [(state, "count"), (p2g, "active_count"), (g2p, "active_count"), (stats, "active_count"), (frame, "count")] {
            wire(&mut graph, (fill, "count"), (node, port));
        }
        wire(&mut graph, (state, "grid_accum"), (zero, "in"));
        wire(&mut graph, (zero, "out"), (p2g, "accum"));
        wire(&mut graph, (state, "out"), (p2g, "points"));
        wire(&mut graph, (p2g, "accum_out"), (update, "accum"));
        wire(&mut graph, (p2g, "accum_out"), (stats, "accum"));
        wire(&mut graph, (state, "grid"), (update, "grid"));
        wire(&mut graph, (state, "out"), (g2p, "points"));
        wire(&mut graph, (update, "grid_out"), (g2p, "grid"));
        wire(&mut graph, (update, "grid_out"), (stats, "grid"));
        wire(&mut graph, (g2p, "points_out"), (stats, "points"));
        wire(&mut graph, (g2p, "points_out"), (state, "in"));
        wire(&mut graph, (state, "stats"), (stats, "stats"));
        wire(&mut graph, (stats, "stats_out"), (state, "stats_in"));
        for node in [p2g, update, g2p] {
            wire(&mut graph, (state, "step_dt"), (node, "step_dt"));
        }
        wire(&mut graph, (state, "tick_end"), (stats, "tick_end"));
        wire(&mut graph, (state, "tick_index"), (stats, "tick_index"));
        wire(&mut graph, (state, "tick_index"), (p2g, "tick_index"));
        wire(&mut graph, (state, "substep_in_tick"), (p2g, "substep_in_tick"));
        wire(&mut graph, (state, "out"), (frame, "points"));
        wire(&mut graph, (state, "stats"), (frame, "stats"));
        graph.add_external_output(frame, "particles_b").expect("frame output");

        let set = |graph: &mut Graph, name: &str, value: ParamValue| {
            graph.set_param(domain, name, value).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        };
        set(&mut graph, "domain_size", ParamValue::Float(settings.domain_size));
        set(&mut graph, "resolution", ParamValue::Float(settings.resolution as f32));
        set(&mut graph, "fill_height", ParamValue::Float(settings.fill_height));
        set(&mut graph, "gravity_x", ParamValue::Float(settings.gravity[0]));
        set(&mut graph, "gravity", ParamValue::Float(settings.gravity[1]));
        set(&mut graph, "gravity_z", ParamValue::Float(settings.gravity[2]));
        set(&mut graph, "stiffness", ParamValue::Float(settings.stiffness));
        set(&mut graph, "cohesion", ParamValue::Float(settings.cohesion));
        set(&mut graph, "liveliness", ParamValue::Float(settings.liveliness));
        set(&mut graph, "seed", ParamValue::Float(settings.seed as f32));
        set(&mut graph, "points_per_cell", ParamValue::Enum(u32::from(settings.points_per_cell_27)));
        for (i, name) in ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"].iter().enumerate() {
            set(&mut graph, name, ParamValue::Bool(settings.closed[i]));
        }
        if let Some(column) = settings.column {
            let t = add(&mut graph, "node.transform_3d");
            for (name, value) in [
                ("pos_x", column.pos[0]), ("pos_y", column.pos[1]), ("pos_z", column.pos[2]),
                ("scale_x", column.scale[0]), ("scale_y", column.scale[1]), ("scale_z", column.scale[2]),
            ] {
                graph.set_param(t, name, ParamValue::Float(value)).expect(name);
            }
            wire(&mut graph, (t, "transform"), (domain, "initial_volume"));
        }

        let plan = compile(&graph).expect("matter scene compiles");
        assert_eq!(plan.substep_regions().len(), 1, "one substep region");
        let harness = harness::shared();
        let device = &harness.device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");
        let output = |node: NodeInstanceId, port: &str| {
            plan.steps()
                .iter()
                .find(|s| s.node == node)
                .and_then(|s| s.outputs.iter().find(|(p, _)| *p == port).map(|&(_, r)| r))
                .unwrap_or_else(|| panic!("no output {port}"))
        };
        let points = output(state, "out");
        let stats_res = output(state, "stats");
        let frame_b = output(frame, "particles_b");
        Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            state: StateStore::new(),
            domain,
            frame_node: frame,
            state_node: state,
            points,
            stats: stats_res,
            frame_b,
            frame_count: 0,
        }
    }

    pub(crate) fn set_domain(&mut self, name: &str, value: f32) {
        self.graph
            .set_param(self.domain, name, ParamValue::Float(value))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }

    /// Run one display frame one fixed tick after the last.
    pub(crate) fn tick(&mut self) {
        self.tick_timed(None);
    }

    /// [`Self::tick`], returning the frame's GPU time; with a sampler, every
    /// dispatch is timed in its own encoder.
    pub(crate) fn tick_timed(&mut self, sampler: Option<&GpuTimestampSampler>) -> GpuFrameProfile {
        let device = &harness::shared().device;
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(self.frame_count) * TICK),
            delta: Seconds(TICK),
            frame_count: i64::from(self.frame_count),
        };
        let mut enc = device.create_encoder("matter-scene");
        if let Some(sampler) = sampler {
            enc.enable_dispatch_profiling(sampler.clone(), device);
        }
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            self.executor
                .execute_frame_with_state(&mut self.graph, &self.plan, time, &mut gpu, &mut self.state, 0);
        }
        self.frame_count += 1;
        enc.commit_and_wait_profiled(device)
    }

    fn read<T: bytemuck::Pod>(&self, res: ResourceId) -> Vec<T> {
        let backend = self.executor.backend();
        let buffer = backend.array_buffer(backend.slot_for(res).expect("bound")).expect("array");
        let ptr = buffer.mapped_ptr().expect("shared storage");
        let n = buffer.size as usize / std::mem::size_of::<T>();
        // SAFETY: the frame completed; `n` whole elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), n).to_vec() }
    }

    /// Live points (id ≠ 0) in storage order.
    pub(crate) fn points(&self) -> Vec<MatterPoint> {
        let count = self.executor.live_scalar_input(self.frame_node, "count").unwrap_or(0.0) as usize;
        let mut all: Vec<MatterPoint> = self.read(self.points);
        all.truncate(count);
        all
    }

    pub(crate) fn stats(&self) -> MatterTickStats {
        let words: Vec<u32> = self.read(self.stats);
        MatterTickStats::from_words(&words[..STATS_WORDS as usize])
    }

    pub(crate) fn frame(&self) -> Vec<FluidParticle> {
        let count = self.executor.live_scalar_input(self.frame_node, "count").unwrap_or(0.0) as usize;
        let mut all: Vec<FluidParticle> = self.read(self.frame_b);
        all.truncate(count);
        all
    }

    /// A scalar the frame node read this frame (a domain output it is wired to).
    pub(crate) fn frame_input(&self, name: &str) -> f32 {
        self.executor
            .live_scalar_input(self.frame_node, name)
            .unwrap_or_else(|| panic!("the frame node read no `{name}`"))
    }

    /// Corrupt one point's position with NaN between frames (the GPU is idle:
    /// every frame waits for completion).
    pub(crate) fn poison_point(&self, index: usize) {
        let backend = self.executor.backend();
        let buffer = backend.array_buffer(backend.slot_for(self.points).expect("bound")).expect("array");
        let offset = (index * std::mem::size_of::<MatterPoint>()) as u64;
        // SAFETY: shared storage, no GPU work in flight.
        unsafe { buffer.write(offset, bytemuck::bytes_of(&[f32::NAN; 3])) };
    }

    /// Give every live point the same velocity and no affine motion, between
    /// frames (the GPU is idle).
    pub(crate) fn set_velocity(&self, velocity: [f32; 3]) {
        let mut points = self.points();
        for p in points.iter_mut().filter(|p| p.id != 0) {
            p.velocity = velocity;
            p.affine_x = [0.0, 0.0, 0.0, p.affine_x[3]];
            p.affine_y = [0.0, 0.0, 0.0, p.affine_y[3]];
            p.affine_z = [0.0; 4];
        }
        let backend = self.executor.backend();
        let buffer = backend.array_buffer(backend.slot_for(self.points).expect("bound")).expect("array");
        // SAFETY: shared storage, no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(&points)) };
    }

    /// A scalar the state node read this frame.
    pub(crate) fn state_input(&self, name: &str) -> f32 {
        self.executor
            .live_scalar_input(self.state_node, name)
            .unwrap_or_else(|| panic!("the state node read no `{name}`"))
    }
}

fn mean_speed(points: &[MatterPoint]) -> f32 {
    let live: Vec<&MatterPoint> = points.iter().filter(|p| p.id != 0).collect();
    live.iter()
        .map(|p| (p.velocity[0].powi(2) + p.velocity[1].powi(2) + p.velocity[2].powi(2)).sqrt())
        .sum::<f32>()
        / live.len().max(1) as f32
}

/// A 1 m box, resolution 32, a 0.5 m pool: after 5 s the pool is still and
/// its bottom quarter compressed by the hydrostatic ρgd/λ (section 12).
#[test]
fn matter_still_pool_settles() {
    let settings = SceneSettings { fill_height: 0.5, ..SceneSettings::default() };
    let mut scene = MatterScene::new(&settings);
    for t in 0..300 {
        scene.tick();
        if t % 30 == 29 && std::env::var_os("MATTER_DIAG").is_some() {
            let pts = scene.points();
            let s = scene.stats();
            let speed = |p: &MatterPoint| (p.velocity[0].powi(2) + p.velocity[1].powi(2) + p.velocity[2].powi(2)).sqrt();
            let top: Vec<&MatterPoint> = pts.iter().filter(|p| p.position[1] > settings.fill_height - 0.0625).collect();
            let deep: Vec<&MatterPoint> = pts.iter().filter(|p| p.position[1] < settings.fill_height * 0.5).collect();
            let mean = |v: &[&MatterPoint]| v.iter().map(|p| speed(p)).sum::<f32>() / v.len().max(1) as f32;
            let expanded = pts.iter().filter(|p| p.volume_ratio > 1.05).count();
            let above = pts.iter().filter(|p| p.position[1] > settings.fill_height + 0.03).count();
            let max_y = pts.iter().map(|p| p.position[1]).fold(f32::MIN, f32::max);
            eprintln!(
                "t={:.2}s mean {:.4} top {:.4} deep {:.4} max {:.3} | J>1.05: {expanded} above surface: {above} max_y {max_y:.3} | E k {:.3} p {:.3} e {:.3} J [{:.4},{:.3}]",
                (t + 1) as f32 / 60.0, mean_speed(&pts), mean(&top), mean(&deep), s.max_speed,
                s.kinetic, s.potential, s.elastic, s.min_j, s.max_j
            );
        }
    }
    let points = scene.points();
    let stats = scene.stats();
    assert_eq!(stats.nonfinite, 0, "{stats:?}");
    let speed = mean_speed(&points);
    eprintln!("matter_still_pool_settles: {} points, mean speed {speed:.4} m/s, J [{:.4}, {:.4}], clamped {}", points.len(), stats.min_j, stats.max_j, stats.clamped);
    assert!(speed < 0.01, "mean speed after 5 s is {speed} m/s");

    // Hydrostatic compression at the bottom quarter's mean depth.
    let floor = 0.0f32;
    let surface = settings.fill_height;
    let bottom: Vec<&MatterPoint> = points
        .iter()
        .filter(|p| p.id != 0 && p.position[1] < floor + 0.25 * (surface - floor))
        .collect();
    let mean_j = bottom.iter().map(|p| f64::from(p.volume_ratio)).sum::<f64>() / bottom.len() as f64;
    let mean_depth = f64::from(surface) - bottom.iter().map(|p| f64::from(p.position[1])).sum::<f64>() / bottom.len() as f64;
    let lambda = manifold_renderer::node_graph::matter::water_lambda(1.0, 1.0);
    let expected = 1.0 - 1000.0 * 9.81 * mean_depth / lambda;
    eprintln!("  bottom quarter: mean J {mean_j:.5}, hydrostatic {expected:.5} at depth {mean_depth:.3} m");
    assert!(
        ((1.0 - mean_j) - (1.0 - expected)).abs() <= 0.2 * (1.0 - expected),
        "bottom-quarter compression {} vs hydrostatic {}",
        1.0 - mean_j,
        1.0 - expected
    );
}

/// A 1 m domain at resolution 32 with a quarter-width, 0.6 m column on a
/// 3 cm pool: a small dam break.
pub(crate) fn small_dam_break() -> SceneSettings {
    SceneSettings {
        fill_height: 0.03,
        column: Some(Transform {
            pos: [-0.375, 0.33, 0.0],
            scale: [0.25, 0.6, 1.0],
            ..Transform::default()
        }),
        ..SceneSettings::default()
    }
}

#[test]
fn matter_frame_ids_strictly_increasing() {
    let mut scene = MatterScene::new(&small_dam_break());
    for _ in 0..30 {
        scene.tick();
    }
    let frame = scene.frame();
    let points = scene.points();
    assert_eq!(frame.len(), points.len());
    let ids: Vec<u32> = frame.iter().map(|p| p.id).filter(|&id| id != 0).collect();
    assert!(!ids.is_empty());
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "frame ids are not strictly increasing");
    // The frame carries the points it was written from, at their rest radius.
    let dx = 1.0f32 / 32.0;
    let radius = (3.0 * dx * dx * dx / 8.0 / (4.0 * std::f32::consts::PI)).cbrt();
    for (f, p) in frame.iter().zip(&points).filter(|(f, _)| f.id != 0) {
        assert_eq!(f.id, p.id);
        assert_eq!(&f.position_radius[..3], &p.position[..]);
        assert!((f.position_radius[3] - radius).abs() < 1e-6);
    }
}

#[test]
fn matter_deterministic_under_seed() {
    let run = |seed: u32, ticks: u32| {
        let mut scene = MatterScene::new(&SceneSettings { seed, ..small_dam_break() });
        for _ in 0..ticks {
            scene.tick();
        }
        scene.points()
    };
    let a = run(7, 120);
    let b = run(7, 120);
    let a_bytes: &[u8] = bytemuck::cast_slice(&a);
    let b_bytes: &[u8] = bytemuck::cast_slice(&b);
    assert!(a_bytes == b_bytes, "two runs of the same seed diverged");
}

#[test]
fn matter_seed_changes_jitter() {
    let first = |seed: u32| {
        let mut scene = MatterScene::new(&SceneSettings { seed, ..small_dam_break() });
        scene.tick();
        scene.points()
    };
    let a = first(0);
    let b = first(1);
    assert_eq!(a.len(), b.len());
    let moved = a.iter().zip(&b).filter(|(p, q)| p.position != q.position).count();
    assert!(moved > a.len() * 9 / 10, "only {moved} of {} points moved with the seed", a.len());
}

#[test]
fn matter_dam_break_energy_bounded() {
    let mut scene = MatterScene::new(&small_dam_break());
    // The first frame starts the epoch and runs no tick; the second runs one.
    scene.tick();
    scene.tick();
    let initial = scene.stats().energy();
    let mut peak = initial;
    for _ in 0..180 {
        scene.tick();
        let stats = scene.stats();
        assert_eq!(stats.nonfinite, 0);
        peak = peak.max(stats.energy());
        assert!(
            stats.energy() <= 1.01 * initial,
            "tick {}: energy {} exceeds 1.01 × initial {initial}",
            stats.tick,
            stats.energy()
        );
    }
    eprintln!("matter_dam_break_energy_bounded: initial {initial:.3} J, peak {peak:.3} J");
}

/// The Dam Break's own setup (4 m, resolution 64, the FLIP preset's column
/// and pool): the largest accumulator stays below 2^30 (section 4.3).
#[test]
fn matter_fixed_point_headroom() {
    let settings = SceneSettings {
        domain_size: 4.0,
        resolution: 64,
        fill_height: 0.16,
        column: Some(Transform {
            pos: [-1.25, 1.12, 0.0],
            scale: [1.18, 1.92, 3.5],
            ..Transform::default()
        }),
        ..SceneSettings::default()
    };
    let mut scene = MatterScene::new(&settings);
    let mut peak = 0u32;
    for t in 0..120 {
        scene.tick();
        let s = scene.stats();
        peak = peak.max(s.max_accum);
        if std::env::var_os("MATTER_DIAG").is_some() && (t % 5 == 0 || s.max_accum >= 1 << 30) {
            eprintln!(
                "tick {t}: max_accum {} max speed {:.3} clamped {} J [{:.4}, {:.4}] live {}",
                s.max_accum, s.max_speed, s.clamped, s.min_j, s.max_j, s.live
            );
        }
    }
    eprintln!("matter_fixed_point_headroom: peak accumulator {peak} ({:.1}% of 2^30)", 100.0 * f64::from(peak) / f64::from(1u32 << 30));
    assert!(peak < 1 << 30, "accumulator reached {peak}");
    assert_eq!(scene.state_input("substeps_per_tick"), 34.0);
}

#[test]
fn matter_nonfinite_tick_not_published() {
    let mut scene = MatterScene::new(&small_dam_break());
    for _ in 0..10 {
        scene.tick();
    }
    let before = scene.frame();
    scene.poison_point(100);
    scene.tick();
    assert!(scene.stats().nonfinite > 0, "a NaN point must show in the stats");
    let published = scene.frame();
    assert!(
        published.iter().all(|p| p.position_radius.iter().all(|v| v.is_finite())),
        "a non-finite tick reached the frame"
    );
    assert_eq!(bytemuck::cast_slice::<FluidParticle, u8>(&published), bytemuck::cast_slice::<FluidParticle, u8>(&before));
    // The readback halts the solver: the state stops advancing.
    scene.tick();
    let halted = scene.points();
    scene.tick();
    assert_eq!(bytemuck::cast_slice::<MatterPoint, u8>(&halted), bytemuck::cast_slice::<MatterPoint, u8>(&scene.points()));
    // Reset starts a fresh epoch that runs again.
    scene.set_domain("reset", 1.0);
    scene.tick();
    scene.tick();
    assert_eq!(scene.stats().nonfinite, 0);
    assert!(scene.points().iter().all(|p| p.position.iter().all(|v| v.is_finite())));
}

/// Live (a preview budget in scope): a non-finite gravity holds the liquid
/// with a named error; on recovery live runs its one-tick allowance, not the
/// held frames' debt.
#[test]
fn matter_domain_holds_on_nonfinite_gravity() {
    let _live = manifold_renderer::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut scene = MatterScene::new(&small_dam_break());
    for _ in 0..5 {
        scene.tick();
    }
    scene.set_domain("gravity", f32::NAN);
    scene.tick();
    let held = scene.points();
    assert_eq!(scene.state_input("ticks"), 0.0);
    scene.tick();
    assert_eq!(bytemuck::cast_slice::<MatterPoint, u8>(&held), bytemuck::cast_slice::<MatterPoint, u8>(&scene.points()));
    scene.set_domain("gravity", -9.81);
    scene.tick();
    assert_eq!(scene.state_input("ticks"), 1.0);
    assert_eq!(scene.stats().nonfinite, 0);
}

#[test]
fn matter_substeps_follow_stiffness_live() {
    let mut scene = MatterScene::new(&SceneSettings { domain_size: 4.0, resolution: 64, fill_height: 0.2, ..SceneSettings::default() });
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 34.0);
    scene.set_domain("stiffness", 0.5);
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 21.0);
    scene.set_domain("stiffness", 2.0);
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 61.0);
    assert!(scene.frame_input("count") > 0.0);
}
