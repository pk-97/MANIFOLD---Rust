//! The whole Live Matter graph on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 3.2): domain → fill → state and its substep region → frame. The
//! harness drives one fixed tick per frame and reads the point state, the
//! per-tick stats and the published frames back.

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuFrameProfile, GpuTextureFormat, GpuTimestampSampler};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::scene::fluid_domain::{domain_layout};
use manifold_nodes_water::fluid::TICK;
use manifold_node_engine::particles::{FluidParticle};
use manifold_nodes_water::fluid_particles::CellRange;
use manifold_nodes_water::liquid::lattice::LiquidLattice;
use manifold_nodes_water::matter::{MatterPoint, MatterTickStats, STATS_WORDS, lattice_blocks};
use manifold_node_engine::{exec::execution_plan::ExecutionPlan, exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, parameters::ParamValue, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId, state_store::StateStore, scene::transform::Transform, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};


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
    /// P2G takes D6's block path: the region sorts the points into the
    /// domain's block bins once per tick and P2G reads the order and ranges.
    pub block_p2g: bool,
    /// Collider roles: built-in unit cubes posed by these transforms (their
    /// scale is the box size), wired as node.fluid_role_source Collider roles.
    pub colliders: Vec<Transform>,
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
            block_p2g: false,
            colliders: Vec::new(),
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
    /// Each collider's node.transform_3d.
    colliders: Vec<NodeInstanceId>,
    solid_b: Option<ResourceId>,
    points: ResourceId,
    stats: ResourceId,
    frame_b: ResourceId,
    /// The cell sort's `order` and `cell_ranges`, on the block path.
    sorted: Option<(ResourceId, ResourceId)>,
    lattice: LiquidLattice,
    frame_count: u32,
    /// Seconds per display frame; one fixed tick unless set.
    frame_interval: f64,
}

const LATTICE: [&str; 7] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z",
];

impl MatterScene {
    pub(crate) fn new(settings: &SceneSettings) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        let domain = add(&mut graph, manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID);
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
        wire(&mut graph, (domain, "momentum_unit"), (p2g, "momentum_unit"));
        wire(&mut graph, (domain, "momentum_unit"), (update, "momentum_unit"));
        wire(&mut graph, (domain, "liveliness"), (g2p, "liveliness"));
        wire(&mut graph, (domain, "cohesion"), (g2p, "cohesion"));

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
        // The coupling sum runs in every region; with no dynamic body it only
        // passes the domain's reaction slot through.
        let reaction = add(&mut graph, "node.matter_body_reaction");
        wire(&mut graph, (update, "grid_out"), (reaction, "grid"));
        wire(&mut graph, (domain, "reaction"), (reaction, "reaction"));
        wire(&mut graph, (reaction, "reaction_out"), (state, "reaction_in"));
        for port in LATTICE.into_iter().chain([
            "gravity_x", "gravity", "gravity_z", "closed_faces", "momentum_unit", "body_count",
            "substeps_per_tick", "dynamic_count",
        ]) {
            wire(&mut graph, (domain, port), (reaction, port));
        }
        for port in ["tick_index", "substep_in_tick", "step_dt"] {
            wire(&mut graph, (state, port), (reaction, port));
        }
        for node in [p2g, update, g2p] {
            wire(&mut graph, (state, "step_dt"), (node, "step_dt"));
        }
        wire(&mut graph, (state, "tick_end"), (stats, "tick_end"));
        wire(&mut graph, (state, "tick_index"), (stats, "tick_index"));
        wire(&mut graph, (state, "tick_index"), (p2g, "tick_index"));
        wire(&mut graph, (state, "substep_in_tick"), (p2g, "substep_in_tick"));
        let sort_node = settings.block_p2g.then(|| add(&mut graph, "node.sort_particles_into_cells"));
        if let Some(sort) = sort_node {
            wire(&mut graph, (state, "out"), (sort, "particles"));
            wire(&mut graph, (fill, "count"), (sort, "count"));
            // Sort once per tick; later substeps reuse its order and ranges.
            wire(&mut graph, (state, "tick_start"), (sort, "enabled"));
            for (from, to) in [
                ("block_center_x", "center_x"), ("block_center_y", "center_y"), ("block_center_z", "center_z"),
                ("block_size_x", "size_x"), ("block_size_y", "size_y"), ("block_size_z", "size_z"),
                ("block_cell_size", "cell_size"),
            ] {
                wire(&mut graph, (domain, from), (sort, to));
            }
            for port in ["blocks_x", "blocks_y", "blocks_z"] {
                wire(&mut graph, (domain, port), (p2g, port));
            }
            wire(&mut graph, (sort, "order"), (p2g, "order"));
            wire(&mut graph, (sort, "cell_ranges"), (p2g, "ranges"));
        }
        wire(&mut graph, (state, "out"), (frame, "points"));
        wire(&mut graph, (state, "stats"), (frame, "stats"));
        let colliders =
            Self::wire_colliders(&mut graph, &registry, settings, [domain, fill, state, update, reaction, g2p, frame]);
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
        let harness = manifold_node_engine::testkit::gpu_harness::shared();
        let device = &harness.device;
        let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
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
        let sorted = sort_node.map(|sort| (output(sort, "order"), output(sort, "cell_ranges")));
        let lattice = LiquidLattice::from_layout(
            &domain_layout(None, settings.domain_size, settings.resolution).expect("scene layout"),
        );
        let solid_b = (!settings.colliders.is_empty()).then(|| output(frame, "solid_b"));
        let mut scene = Self {
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
            sorted,
            lattice,
            frame_count: 0,
            frame_interval: TICK,
            colliders,
            solid_b,
        };
        for (index, transform) in settings.colliders.iter().enumerate() {
            scene.set_collider(index, *transform);
        }
        scene
    }

    /// Collider roles as the presets wire them: a transform into a built-in
    /// unit cube role source into the domain, which the fill seeds around;
    /// node.matter_move_bodies in the
    /// region feeding the grid update; node.liquid_solid_distance feeding the
    /// frame's solid lattice. Returns each collider's transform node.
    fn wire_colliders(
        graph: &mut Graph,
        registry: &PrimitiveRegistry,
        settings: &SceneSettings,
        [domain, fill, state, update, reaction, g2p, frame]: [NodeInstanceId; 7],
    ) -> Vec<NodeInstanceId> {
        if settings.colliders.is_empty() {
            return Vec::new();
        }
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        fn wire(graph: &mut Graph, from: (NodeInstanceId, &'static str), to: (NodeInstanceId, &'static str)) {
            graph.connect(from, to).unwrap_or_else(|e| panic!("{from:?} -> {to:?}: {e:?}"));
        }
        const ROLES: [&str; 4] = ["role_0", "role_1", "role_2", "role_3"];
        let set = |graph: &mut Graph, node: NodeInstanceId, name: &str, value: ParamValue| {
            graph.set_param(node, name, value).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        };
        let mut transforms = Vec::new();
        for role in ROLES.iter().take(settings.colliders.len()) {
            let transform = add(graph, "node.transform_3d");
            let source = add(graph, "node.fluid_role_source");
            set(graph, source, "role", ParamValue::Enum(3));
            set(graph, source, "shape", ParamValue::Enum(1));
            // A built-in cube of this radius spans ±0.5, so the transform's
            // scale is the box size.
            set(graph, source, "radius", ParamValue::Float(0.5 / 0.577_350_26));
            wire(graph, (transform, "transform"), (source, "transform"));
            wire(graph, (source, "role"), (domain, role));
            transforms.push(transform);
        }
        // The fill leaves seeds inside a collider's starting pose unused.
        for port in ["bodies", "shapes", "atlas", "body_count", "epoch"] {
            wire(graph, (domain, port), (fill, port));
        }
        let bodies = add(graph, "node.matter_move_bodies");
        wire(graph, (domain, "bodies"), (bodies, "bodies"));
        for (from, to) in [
            ("first_tick", "first_tick"), ("body_count", "body_count"), ("body_rows", "rows"),
            ("reaction", "reaction"), ("substeps_per_tick", "substeps_per_tick"),
            ("momentum_unit", "momentum_unit"), ("cell_size", "cell_size"), ("dynamic_count", "dynamic_count"),
        ] {
            wire(graph, (domain, from), (bodies, to));
        }
        for port in ["tick_index", "substep_in_tick", "step_dt"] {
            wire(graph, (state, port), (bodies, port));
        }
        wire(graph, (bodies, "bodies_out"), (update, "bodies"));
        wire(graph, (bodies, "bodies_out"), (reaction, "bodies"));
        for port in ["shapes", "atlas"] {
            wire(graph, (domain, port), (reaction, port));
        }
        for port in ["shapes", "atlas", "body_count", "lattice_min_x", "lattice_min_y", "lattice_min_z"] {
            wire(graph, (domain, port), (update, port));
        }
        wire(graph, (bodies, "bodies_out"), (g2p, "bodies"));
        for port in ["shapes", "atlas", "body_count"] {
            wire(graph, (domain, port), (g2p, port));
        }
        let solid = add(graph, "node.liquid_solid_distance");
        for port in [
            "bodies", "shapes", "atlas", "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size",
            "nodes_x", "nodes_y", "nodes_z", "closed_faces", "body_count",
        ] {
            wire(graph, (domain, port), (solid, port));
        }
        wire(graph, (domain, "body_rows"), (solid, "rows"));
        wire(graph, (solid, "solid"), (frame, "solid"));
        graph.add_external_output(frame, "solid_b").expect("solid output");
        transforms
    }

    /// The frame's solid lattice B (walls and colliders), with colliders.
    pub(crate) fn solid_b(&self) -> Vec<f32> {
        self.read(self.solid_b.expect("a scene with colliders"))
    }

    pub(crate) fn lattice(&self) -> LiquidLattice {
        self.lattice
    }

    /// Simulated seconds at the end of the last frame's ticks; 0 before any frame.
    pub(crate) fn simulation_time(&self) -> f32 {
        self.executor.live_scalar_input(self.frame_node, "simulation_time").unwrap_or(0.0)
    }

    /// Pose collider `index` from now on (its node.transform_3d).
    pub(crate) fn set_collider(&mut self, index: usize, transform: Transform) {
        let node = self.colliders[index];
        for (name, value) in [
            ("pos_x", transform.pos[0]), ("pos_y", transform.pos[1]), ("pos_z", transform.pos[2]),
            ("rot_x", transform.rot_euler[0]), ("rot_y", transform.rot_euler[1]), ("rot_z", transform.rot_euler[2]),
            ("scale_x", transform.scale[0]), ("scale_y", transform.scale[1]), ("scale_z", transform.scale[2]),
        ] {
            self.graph.set_param(node, name, ParamValue::Float(value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

    pub(crate) fn set_domain(&mut self, name: &str, value: f32) {
        self.graph
            .set_param(self.domain, name, ParamValue::Float(value))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }

    /// Display frames `interval` seconds apart from now on (two ticks per
    /// frame at 30 Hz).
    pub(crate) fn set_frame_interval(&mut self, interval: f64) {
        self.frame_interval = interval;
    }

    /// Run one display frame one fixed tick after the last.
    pub(crate) fn tick(&mut self) {
        self.tick_timed(None);
    }

    /// [`Self::tick`], returning the frame's GPU time; with a sampler, every
    /// dispatch is timed in its own encoder.
    pub(crate) fn tick_timed(&mut self, sampler: Option<&GpuTimestampSampler>) -> GpuFrameProfile {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(self.frame_count) * self.frame_interval),
            delta: Seconds(self.frame_interval),
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
        let buffer = self
            .executor
            .host_array_buffer(&self.graph, &self.plan, res)
            .expect("array holds its own contents");
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

    /// On the block path, after a frame: the fraction of live points whose
    /// stencil has left the tile of the block the tick's sort put them in, so
    /// P2G adds them globally. The last substep sees about this much drift;
    /// earlier substeps see less.
    pub(crate) fn tile_drift(&self) -> f64 {
        let (order, ranges) = self.sorted.expect("the block path");
        let points = self.points();
        let order: Vec<u32> = self.read(order);
        let ranges: Vec<CellRange> = self.read(ranges);
        let blocks = lattice_blocks(&self.lattice);
        let (mut live, mut left) = (0u64, 0u64);
        for (bin, range) in ranges.iter().take(blocks.iter().product::<u32>() as usize).enumerate() {
            let bin = bin as u32;
            let block = [bin % blocks[0], (bin / blocks[0]) % blocks[1], bin / (blocks[0] * blocks[1])].map(i64::from);
            for &index in &order[range.start as usize..(range.start + range.count) as usize] {
                let Some(point) = points.get(index as usize).filter(|p| p.id != 0) else {
                    continue;
                };
                let base: [i64; 3] = std::array::from_fn(|axis| {
                    ((point.position[axis] - self.lattice.min()[axis]) / self.lattice.cell_size() - 0.5).floor() as i64
                });
                live += 1;
                left += u64::from((0..3).any(|axis| !(0..=3).contains(&(base[axis] - block[axis] * 4))));
            }
        }
        left as f64 / live.max(1) as f64
    }
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

/// Unequal Speed-history intervals must keep their own subdivision count and
/// fixed-point unit when export groups both into one display frame.
#[test]
fn matter_variable_speed_export_grouping_matches_raw_points() {
    use manifold_nodes_water::matter::{MAX_SUBSTEPS, WATER_DENSITY, free_fall_speed, momentum_unit, substeps_for_interval, substeps_per_tick, water_lambda, wave_speed};
    use manifold_nodes_water::physics::PhysicsStepScope;

    let _offline = PhysicsStepScope::for_render(true);
    let settings = SceneSettings { resolution: 16, domain_size: 1.0, stiffness: 1.0, ..SceneSettings::default() };
    let dx = settings.domain_size / settings.resolution as f32;
    let nominal = substeps_per_tick(
        dx,
        wave_speed(water_lambda(f64::from(settings.domain_size), f64::from(settings.stiffness)),
            f64::from(WATER_DENSITY)) as f32,
        free_fall_speed(f64::from(settings.domain_size)) as f32,
        None, None,
    );
    let expected = [0.25, 4.0].map(|speed| {
        let duration = speed * TICK;
        let subdivisions = substeps_for_interval(duration as f32, nominal).min(MAX_SUBSTEPS);
        (subdivisions, momentum_unit(dx, duration / f64::from(subdivisions)))
    });
    assert_eq!(expected, [(5, 128.0), (68, 64.0)]);
    assert_ne!(expected[0].0, expected[1].0, "fixture must vary subdivision counts");
    assert_ne!(expected[0].1, expected[1].1, "fixture must vary momentum units");

    let run = |grouped| {
        let mut scene = MatterScene::new(&settings);
        scene.set_domain("speed", 0.25);
        scene.tick(); // Seed at transport 0, before either accepted interval.
        assert_eq!(scene.state_input("ticks"), 0.0);
        scene.set_domain("speed", 4.0);
        let domain_step = scene.plan.steps().iter().position(|step| step.node == scene.domain).unwrap();
        let mut mask = vec![false; scene.plan.steps().len()];
        let mut params = vec![None; scene.plan.steps().len()];
        mask[domain_step] = true;
        params[domain_step] = Some(scene.graph.get_node(scene.domain).unwrap().params.clone());
        // Observe the edit at TICK without accepting work, including in the
        // grouped run whose next full render is at 2*TICK.
        manifold_nodes_water::runtime::physics_sampling::execute_physics_sample_frame(
            &mut scene.executor,
            &mut scene.graph, &scene.plan,
            FrameTime { beats: Beats(0.0), seconds: Seconds(TICK), delta: Seconds(0.0), frame_count: 1 },
            &mask, &params,
        );
        let interval_settings = |scene: &MatterScene, iteration| {
            let output = scene.graph.get_node(scene.domain).unwrap().node
                .substep_clock_interval(iteration).expect("accepted interval metadata");
            let unit = output.scalars.iter().find(|(port, _)| *port == "momentum_unit").unwrap().1;
            (output.timing.iterations, unit)
        };
        let ticks = if grouped {
            scene.set_frame_interval(2.0 * TICK);
            scene.tick_timed(None);
            assert_eq!(interval_settings(&scene, 0), expected[0]);
            assert_eq!(interval_settings(&scene, expected[0].0), expected[1]);
            scene.state_input("ticks")
        } else {
            scene.tick_timed(None);
            assert_eq!(interval_settings(&scene, 0), expected[0]);
            let first_ticks = scene.state_input("ticks");
            scene.tick_timed(None);
            assert_eq!(interval_settings(&scene, 0), expected[1]);
            first_ticks + scene.state_input("ticks")
        };
        assert_eq!(ticks, 2.0);
        let points = scene.points();
        let stats = scene.stats();
        assert!(!points.is_empty());
        assert!(points.iter().any(|point| point.id != 0), "fixture must contain live points");
        assert!(points.iter().all(|point| point.position.iter().chain(&point.velocity)
            .chain(std::iter::once(&point.volume_ratio)).chain(&point.affine_x)
            .chain(&point.affine_y).chain(&point.affine_z).all(|value| value.is_finite())));
        assert_eq!(stats.nonfinite, 0);
        assert!(stats.live > 0);
        assert_eq!(stats.tick, 1, "two completed ticks have zero-based final index 1");
        assert_eq!(scene.simulation_time().to_bits(), (((0.25 + 4.0) * TICK) as f32).to_bits());
        (points, stats, scene.simulation_time())
    };
    let (grouped, grouped_stats, grouped_time) = run(true);
    let (separate, separate_stats, separate_time) = run(false);
    assert_eq!(grouped.len(), separate.len());
    assert!(bytemuck::cast_slice::<MatterPoint, u8>(&grouped)
        == bytemuck::cast_slice::<MatterPoint, u8>(&separate),
        "raw Matter points differ between grouped and separate export frames");
    assert_eq!(grouped_stats, separate_stats);
    assert_eq!(grouped_time.to_bits(), separate_time.to_bits());
}

/// D6's block path sorts once per tick and reuses that order for every
/// substep, so points drift out of their block's tile; the integer
/// accumulator still makes the whole run bit-identical to the per-point path.
#[test]
fn matter_block_path_matches_per_point() {
    let run = |block_p2g: bool| {
        let mut scene = MatterScene::new(&SceneSettings { block_p2g, ..small_dam_break() });
        let mut left_tile = 0.0f64;
        for _ in 0..120 {
            scene.tick();
            if block_p2g {
                left_tile = left_tile.max(scene.tile_drift());
            }
        }
        assert_eq!(scene.stats().nonfinite, 0);
        (scene.points(), left_tile)
    };
    let (point, _) = run(false);
    let (block, left_tile) = run(true);
    eprintln!("  at tick end, at most {left_tile:.4} of points had left their sorted block's tile");
    assert!(left_tile > 0.0, "the scene never took the global path");
    assert_eq!(point.len(), block.len());
    let differ = point
        .iter()
        .zip(&block)
        .filter(|(a, b)| bytemuck::bytes_of(*a) != bytemuck::bytes_of(*b))
        .count();
    eprintln!("matter_block_path_matches_per_point: {} points, {differ} differ after 120 ticks", point.len());
    assert_eq!(differ, 0, "the block path diverged from the per-point path");
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

/// Live (a preview budget in scope): a non-finite gravity holds the liquid
/// with a named error; recovery accepts at most two fixed intervals and
/// discards excess elapsed time, so the next frame owes only its own tick.
#[test]
fn matter_domain_holds_on_nonfinite_gravity() {
    let _live = manifold_nodes_water::physics::PhysicsStepScope::for_render(false);
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
    assert_eq!(scene.state_input("ticks"), 2.0);
    assert_eq!(scene.stats().nonfinite, 0);
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
