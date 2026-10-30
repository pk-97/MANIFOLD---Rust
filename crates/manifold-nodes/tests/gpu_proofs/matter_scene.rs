//! The whole Live Matter graph on the GPU:
//! domain → fill → state and its substep region → frame. The
//! harness drives one fixed tick per frame and reads the point state, the
//! per-tick stats and the published frames back.

use manifold_core::fluid_domain::domain_layout;
use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuFrameProfile, GpuTextureFormat, GpuTimestampSampler};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::{
    exec::effect_node::FrameTime, exec::effect_node::NodeInstanceId, exec::execution::Executor,
    exec::execution_plan::ExecutionPlan, exec::execution_plan::ResourceId,
    exec::execution_plan::compile, exec::metal_backend::MetalBackend, graph::Graph,
    load::graph_loader::pre_allocate_resources, parameters::ParamValue,
    persistence::PrimitiveRegistry, scene::transform::Transform, state_store::StateStore,
};
use manifold_physics::clock::TICK;
use manifold_water_gpu_mpm::matter::{MatterPoint, MatterTickStats, STATS_WORDS};

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
    pub seed: u32,
    pub points_per_cell_27: bool,
    pub closed: [bool; 6],
    /// Colliders: built-in unit cubes posed by these transforms (their scale
    /// is the box size), bodies of a Physics World coupled with the liquid.
    pub colliders: Vec<Transform>,
    /// The colliders' Body Type: Fixed (0) or Animated (2).
    pub collider_motion: u32,
    /// Colliders load this mesh file instead of the built-in cube; the
    /// transforms' scale scales it.
    pub collider_mesh: Option<std::path::PathBuf>,
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
            seed: 0,
            points_per_cell_27: false,
            closed: [true; 6],
            colliders: Vec::new(),
            collider_motion: 0,
            collider_mesh: None,
        }
    }
}

pub(crate) struct MatterScene {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    state: StateStore,
    pub domain: NodeInstanceId,
    /// The node.matter_material whose dials the points are made of.
    pub material: NodeInstanceId,
    pub frame_node: NodeInstanceId,
    pub state_node: NodeInstanceId,
    /// Each collider's node.transform_3d.
    colliders: Vec<NodeInstanceId>,
    points: ResourceId,
    stats: ResourceId,
    frame_b: ResourceId,
    /// The cell size every Matter atom reads, in metres.
    cell_size: f32,
    frame_count: u32,
    /// Seconds per display frame; one fixed tick unless set.
    frame_interval: f64,
}

const BOUNDS: [&str; 6] = [
    "bounds_lo_x",
    "bounds_lo_y",
    "bounds_lo_z",
    "bounds_hi_x",
    "bounds_hi_y",
    "bounds_hi_z",
];

const WALL_VELOCITY: [&str; 6] = [
    "wall_velocity_lo_x",
    "wall_velocity_lo_y",
    "wall_velocity_lo_z",
    "wall_velocity_hi_x",
    "wall_velocity_hi_y",
    "wall_velocity_hi_z",
];

const FILL_BOX: [&str; 7] = [
    "fill_min_x",
    "fill_min_y",
    "fill_min_z",
    "cell_size",
    "fill_cells_x",
    "fill_cells_y",
    "fill_cells_z",
];

impl MatterScene {
    pub(crate) fn new(settings: &SceneSettings) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        let domain = add(
            &mut graph,
            manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID,
        );
        let fill = add(&mut graph, "node.matter_fill");
        let state = add(&mut graph, "node.matter_state");
        let p2g = add(&mut graph, "node.matter_to_grid");
        let solid = add(&mut graph, "node.matter_static_solid");
        let update = add(&mut graph, "node.matter_grid_update");
        let g2p = add(&mut graph, "node.grid_to_matter");
        let stats = add(&mut graph, "node.matter_stats");
        let frame = add(&mut graph, "node.matter_frame");
        let material = add(&mut graph, "node.matter_material");
        fn wire(
            graph: &mut Graph,
            from: (NodeInstanceId, &'static str),
            to: (NodeInstanceId, &'static str),
        ) {
            graph
                .connect(from, to)
                .unwrap_or_else(|e| panic!("{from:?} -> {to:?}: {e:?}"));
        }
        for port in FILL_BOX {
            wire(&mut graph, (domain, port), (fill, port));
        }
        for port in ["field_min_x", "field_min_y", "field_min_z"] {
            wire(&mut graph, (domain, port), (stats, port));
        }
        for node in [p2g, update, g2p, solid] {
            wire(&mut graph, (domain, "cell_size"), (node, "cell_size"));
        }
        let reaction = add(&mut graph, "node.matter_body_reaction");
        for node in [p2g, update, reaction, g2p, stats, state, solid] {
            wire(&mut graph, (domain, "pool_tiles"), (node, "pool_tiles"));
        }
        wire(&mut graph, (p2g, "tiles"), (stats, "tiles"));
        for port in ["tiles", "held", "fresh", "held_args", "built"] {
            wire(&mut graph, (p2g, port), (solid, port));
        }
        for node in [update, reaction] {
            wire(&mut graph, (p2g, "held"), (node, "held"));
            wire(&mut graph, (p2g, "held_args"), (node, "held_args"));
        }
        for node in [update, reaction, g2p] {
            wire(&mut graph, (solid, "solid"), (node, "static_solid"));
            wire(&mut graph, (p2g, "tiles"), (node, "tiles"));
            for port in BOUNDS {
                wire(&mut graph, (domain, port), (node, port));
            }
        }
        for node in [update, reaction] {
            wire(&mut graph, (domain, "closed_faces"), (node, "wall_faces"));
            for port in WALL_VELOCITY {
                wire(&mut graph, (domain, port), (node, port));
            }
        }
        for port in ["open_faces", "kill_height"] {
            wire(&mut graph, (domain, port), (g2p, port));
        }
        for port in ["gravity_x", "gravity", "gravity_z"] {
            wire(&mut graph, (domain, port), (update, port));
            wire(&mut graph, (domain, port), (stats, port));
        }
        for port in [
            "pool_cells",
            "fill_points",
            "fills",
            "points_per_cell",
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
        for node in [state, p2g, g2p, stats] {
            wire(&mut graph, (material, "materials"), (node, "materials"));
        }
        for port in ["wave", "snow_wave", "diffusivity", "density", "wall_friction"] {
            wire(&mut graph, (material, port), (domain, port));
        }
        for port in ["wave_reach", "speed_estimate"] {
            wire(&mut graph, (domain, port), (g2p, port));
        }
        wire(&mut graph, (state, "hardened"), (domain, "hardened"));
        for node in [update, reaction] {
            wire(
                &mut graph,
                (domain, "wall_friction"),
                (node, "wall_friction"),
            );
        }
        wire(
            &mut graph,
            (domain, "momentum_unit"),
            (p2g, "momentum_unit"),
        );
        wire(
            &mut graph,
            (domain, "momentum_unit"),
            (update, "momentum_unit"),
        );

        wire(&mut graph, (fill, "points"), (state, "seed"));
        for (node, port) in [
            (state, "count"),
            (p2g, "active_count"),
            (g2p, "active_count"),
            (stats, "active_count"),
            (frame, "count"),
        ] {
            wire(&mut graph, (fill, "count"), (node, port));
        }
        wire(&mut graph, (state, "grid_accum"), (p2g, "accum"));
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
        wire(&mut graph, (update, "grid_out"), (reaction, "grid"));
        wire(&mut graph, (domain, "reaction"), (reaction, "reaction"));
        wire(
            &mut graph,
            (reaction, "reaction_out"),
            (state, "reaction_in"),
        );
        for port in [
            "field_min_x",
            "field_min_y",
            "field_min_z",
            "cell_size",
            "gravity_x",
            "gravity",
            "gravity_z",
            "momentum_unit",
            "body_count",
            "substeps_per_tick",
            "dynamic_count",
        ] {
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
        wire(
            &mut graph,
            (state, "substep_in_tick"),
            (p2g, "substep_in_tick"),
        );
        wire(&mut graph, (state, "out"), (frame, "points"));
        wire(&mut graph, (state, "stats"), (frame, "stats"));
        let colliders = Self::wire_colliders(
            &mut graph,
            &registry,
            settings,
            [domain, fill, state, update, reaction, g2p, solid],
        );
        graph
            .add_external_output(frame, "particles_b")
            .expect("frame output");

        let cell_size = f64::from(settings.domain_size) / f64::from(settings.resolution);
        let set = |graph: &mut Graph, name: &str, value: ParamValue| {
            graph
                .set_param(domain, name, value)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        };
        set(
            &mut graph,
            "domain_size",
            ParamValue::Float(settings.domain_size),
        );
        set(
            &mut graph,
            "cell_metres",
            ParamValue::Float(cell_size as f32),
        );
        set(
            &mut graph,
            "fill_height",
            ParamValue::Float(settings.fill_height),
        );
        set(
            &mut graph,
            "gravity_x",
            ParamValue::Float(settings.gravity[0]),
        );
        set(
            &mut graph,
            "gravity",
            ParamValue::Float(settings.gravity[1]),
        );
        set(
            &mut graph,
            "gravity_z",
            ParamValue::Float(settings.gravity[2]),
        );
        for (name, value) in [("stiffness", settings.stiffness), ("cohesion", settings.cohesion)] {
            graph
                .set_param(material, name, ParamValue::Float(value))
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
        set(&mut graph, "seed", ParamValue::Float(settings.seed as f32));
        set(
            &mut graph,
            "points_per_cell",
            ParamValue::Enum(u32::from(settings.points_per_cell_27)),
        );
        for (i, name) in [
            "closed_neg_x",
            "closed_pos_x",
            "closed_neg_y",
            "closed_pos_y",
            "closed_neg_z",
            "closed_pos_z",
        ]
        .iter()
        .enumerate()
        {
            set(&mut graph, name, ParamValue::Bool(settings.closed[i]));
        }
        if let Some(column) = settings.column {
            let t = add(&mut graph, "node.transform_3d");
            for (name, value) in [
                ("pos_x", column.pos[0]),
                ("pos_y", column.pos[1]),
                ("pos_z", column.pos[2]),
                ("scale_x", column.scale[0]),
                ("scale_y", column.scale[1]),
                ("scale_z", column.scale[2]),
            ] {
                graph
                    .set_param(t, name, ParamValue::Float(value))
                    .expect(name);
            }
            let fill = add(&mut graph, "node.fluid_role_source");
            graph
                .set_param(fill, "role", ParamValue::Enum(0))
                .expect("role");
            wire(&mut graph, (t, "transform"), (fill, "transform"));
            // A role input the default count holds.
            wire(&mut graph, (fill, "role"), (domain, "role_0"));
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
        let cell_size = domain_layout(None, settings.domain_size, cell_size)
            .expect("scene layout")
            .cell_size as f32;
        let mut scene = Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            state: StateStore::new(),
            domain,
            material,
            frame_node: frame,
            state_node: state,
            points,
            stats: stats_res,
            frame_b,
            cell_size,
            frame_count: 0,
            frame_interval: TICK,
            colliders,
        };
        for (index, transform) in settings.colliders.iter().enumerate() {
            scene.set_collider(index, *transform);
        }
        scene
    }

    /// Colliders as a scene wires them: a transform and a unit cube into a
    /// body of a Physics World coupled with the domain, which the fill seeds
    /// around; node.matter_move_bodies in the region feeding the grid update;
    /// Returns each collider's transform node.
    fn wire_colliders(
        graph: &mut Graph,
        registry: &PrimitiveRegistry,
        settings: &SceneSettings,
        [domain, fill, state, update, reaction, g2p, solid]: [NodeInstanceId; 7],
    ) -> Vec<NodeInstanceId> {
        if settings.colliders.is_empty() {
            return Vec::new();
        }
        let add = |graph: &mut Graph, id: &str| graph.add_node(registry.construct(id).expect(id));
        fn wire(
            graph: &mut Graph,
            from: (NodeInstanceId, &'static str),
            to: (NodeInstanceId, &'static str),
        ) {
            graph
                .connect(from, to)
                .unwrap_or_else(|e| panic!("{from:?} -> {to:?}: {e:?}"));
        }
        const BODIES: [&str; 4] = ["body_0", "body_1", "body_2", "body_3"];
        let set = |graph: &mut Graph, node: NodeInstanceId, name: &str, value: ParamValue| {
            graph
                .set_param(node, name, value)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        };
        let world = add(graph, "node.physics_world");
        let mut colliders = RigidImpulseTargets::default();
        let mut transforms = Vec::new();
        for (index, port) in BODIES.iter().take(settings.colliders.len()).enumerate() {
            let transform = add(graph, "node.transform_3d");
            let body = add(graph, "node.rigid_body");
            set(
                graph,
                body,
                "motion",
                ParamValue::Enum(settings.collider_motion),
            );
            if let Some(path) = &settings.collider_mesh {
                set(
                    graph,
                    body,
                    "path",
                    ParamValue::String(std::sync::Arc::new(
                        path.to_str().expect("utf-8 path").to_owned(),
                    )),
                );
            } else {
                let cube = add(graph, "node.cube_mesh");
                wire(graph, (cube, "source"), (body, "source"));
            }
            wire(graph, (transform, "transform"), (body, "transform"));
            wire(graph, (body, "body"), (world, port));
            graph
                .add_external_output(body, "pose")
                .expect("body pose output");
            colliders = colliders.union(RigidImpulseTargets::body(index).expect("a body slot"));
            transforms.push(transform);
        }
        manifold_nodes_water::testkit::couple(graph, domain, world, colliders)
            .expect("couple the liquid with its bodies");
        // The fill leaves seeds inside a collider's starting pose unused.
        for port in [
            "bodies",
            "shapes",
            "atlas",
            "collider_nodes",
            "collider_triangles",
            "body_count",
            "epoch",
        ] {
            wire(graph, (domain, port), (fill, port));
        }
        let bodies = add(graph, "node.matter_move_bodies");
        wire(graph, (domain, "bodies"), (bodies, "bodies"));
        for (from, to) in [
            ("first_tick", "first_tick"),
            ("body_count", "body_count"),
            ("body_rows", "rows"),
            ("reaction", "reaction"),
            ("substeps_per_tick", "substeps_per_tick"),
            ("momentum_unit", "momentum_unit"),
            ("cell_size", "cell_size"),
            ("dynamic_count", "dynamic_count"),
        ] {
            wire(graph, (domain, from), (bodies, to));
        }
        for port in ["tick_index", "substep_in_tick", "step_dt"] {
            wire(graph, (state, port), (bodies, port));
        }
        wire(graph, (bodies, "bodies_out"), (update, "bodies"));
        wire(graph, (bodies, "bodies_out"), (reaction, "bodies"));
        wire(graph, (bodies, "bodies_out"), (solid, "bodies"));
        for port in [
            "shapes",
            "collider_nodes",
            "collider_triangles",
            "body_count",
            "static_key",
        ] {
            wire(graph, (domain, port), (solid, port));
        }
        for port in ["shapes", "atlas"] {
            wire(graph, (domain, port), (reaction, port));
        }
        for port in [
            "shapes",
            "atlas",
            "body_count",
            "field_min_x",
            "field_min_y",
            "field_min_z",
        ] {
            wire(graph, (domain, port), (update, port));
        }
        wire(graph, (bodies, "bodies_out"), (g2p, "bodies"));
        for port in ["shapes", "atlas", "body_count"] {
            wire(graph, (domain, port), (g2p, port));
        }
        transforms
    }

    pub(crate) fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// Simulated seconds at the end of the last frame's ticks; 0 before any frame.
    pub(crate) fn simulation_time(&self) -> f32 {
        self.executor
            .live_scalar_input(self.frame_node, "simulation_time")
            .unwrap_or(0.0)
    }

    /// Pose collider `index` from now on (its node.transform_3d).
    pub(crate) fn set_collider(&mut self, index: usize, transform: Transform) {
        let node = self.colliders[index];
        for (name, value) in [
            ("pos_x", transform.pos[0]),
            ("pos_y", transform.pos[1]),
            ("pos_z", transform.pos[2]),
            ("rot_x", transform.rot_euler[0]),
            ("rot_y", transform.rot_euler[1]),
            ("rot_z", transform.rot_euler[2]),
            ("scale_x", transform.scale[0]),
            ("scale_y", transform.scale[1]),
            ("scale_z", transform.scale[2]),
        ] {
            self.graph
                .set_param(node, name, ParamValue::Float(value))
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

    pub(crate) fn set_domain(&mut self, name: &str, value: f32) {
        self.graph
            .set_param(self.domain, name, ParamValue::Float(value))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }

    pub(crate) fn set_material(&mut self, name: &str, value: f32) {
        self.graph
            .set_param(self.material, name, ParamValue::Float(value))
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
            self.executor.execute_frame_with_state(
                &mut self.graph,
                &self.plan,
                time,
                &mut gpu,
                &mut self.state,
                0,
            );
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
        let count = self
            .executor
            .live_scalar_input(self.frame_node, "count")
            .unwrap_or(0.0) as usize;
        let mut all: Vec<MatterPoint> = self.read(self.points);
        all.truncate(count);
        all
    }

    pub(crate) fn stats(&self) -> MatterTickStats {
        let words: Vec<u32> = self.read(self.stats);
        MatterTickStats::from_words(&words[..STATS_WORDS as usize])
    }

    pub(crate) fn frame(&self) -> Vec<FluidParticle> {
        let count = self
            .executor
            .live_scalar_input(self.frame_node, "count")
            .unwrap_or(0.0) as usize;
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

    /// A scalar the state node read this frame.
    pub(crate) fn state_input(&self, name: &str) -> f32 {
        self.executor
            .live_scalar_input(self.state_node, name)
            .unwrap_or_else(|| panic!("the state node read no `{name}`"))
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
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "frame ids are not strictly increasing"
    );
    // The frame carries the points it was written from, at their rest radius.
    let dx = 1.0f32 / 32.0;
    let radius = (3.0 * dx * dx * dx / 8.0 / (4.0 * std::f32::consts::PI)).cbrt();
    for (f, p) in frame.iter().zip(&points).filter(|(f, _)| f.id != 0) {
        assert_eq!(f.id, p.id);
        assert_eq!(&f.position_radius[..3], &p.position[..]);
        assert!((f.position_radius[3] - radius).abs() < 1e-6);
    }
}

/// Unequal Speed-history intervals must keep their own subdivision count and
/// fixed-point unit when export groups both into one display frame.
#[test]
fn matter_variable_speed_export_grouping_matches_raw_points() {
    use manifold_water_gpu_mpm::matter::{
        MAX_SUBSTEPS, MatterMaterial, SAND_DENSITY, SAND_FRICTION_ANGLE, free_fall_speed,
        momentum_unit, substeps_for_interval, substeps_per_tick,
    };

    // Stiffness 2.5: the slow interval's substeps cross a power of two of
    // dx/dt that the fast interval's capped substeps do not.
    let settings = SceneSettings {
        resolution: 16,
        domain_size: 1.0,
        stiffness: 2.5,
        ..SceneSettings::default()
    };
    let dx = settings.domain_size / settings.resolution as f32;
    let nominal = substeps_per_tick(
        dx,
        MatterMaterial::sand(
            f64::from(settings.stiffness),
            f64::from(SAND_FRICTION_ANGLE),
            0.0,
        )
        .wave_speed(f64::from(SAND_DENSITY)) as f32,
        free_fall_speed(f64::from(settings.domain_size)) as f32,
        None,
        None,
    );
    let expected = [0.25, 4.0].map(|speed| {
        let duration = speed * TICK;
        let subdivisions = substeps_for_interval(duration as f32, nominal).min(MAX_SUBSTEPS);
        (
            subdivisions,
            momentum_unit(dx, duration / f64::from(subdivisions)),
        )
    });
    assert_ne!(
        expected[0].0, expected[1].0,
        "fixture must vary subdivision counts"
    );
    assert_ne!(
        expected[0].1, expected[1].1,
        "fixture must vary momentum units"
    );

    let run = |grouped| {
        let mut scene = MatterScene::new(&settings);
        scene.set_domain("speed", 0.25);
        scene.tick(); // Seed at transport 0, before either accepted interval.
        assert_eq!(scene.state_input("ticks"), 0.0);
        scene.set_domain("speed", 4.0);
        let domain_step = scene
            .plan
            .steps()
            .iter()
            .position(|step| step.node == scene.domain)
            .unwrap();
        let mut mask = vec![false; scene.plan.steps().len()];
        let mut params = vec![None; scene.plan.steps().len()];
        mask[domain_step] = true;
        params[domain_step] = Some(scene.graph.get_node(scene.domain).unwrap().params.clone());
        // Observe the edit at TICK without accepting work, including in the
        // grouped run whose next full render is at 2*TICK.
        manifold_nodes_water::runtime::physics_sampling::execute_physics_sample_frame(
            &mut scene.executor,
            &mut scene.graph,
            &scene.plan,
            FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(TICK),
                delta: Seconds(0.0),
                frame_count: 1,
            },
            &mask,
            &params,
            manifold_water_rigid::physics::SimStep::default(),
        );
        let interval_settings = |scene: &MatterScene, iteration| {
            let output = scene
                .graph
                .get_node(scene.domain)
                .unwrap()
                .node
                .substep_clock_interval(iteration)
                .expect("accepted interval metadata");
            let unit = output
                .scalars
                .iter()
                .find(|(port, _)| *port == "momentum_unit")
                .unwrap()
                .1;
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
        assert!(
            points.iter().any(|point| point.id != 0),
            "fixture must contain live points"
        );
        assert!(points.iter().all(|point| {
            point
                .position
                .iter()
                .chain(&point.velocity)
                .chain(std::iter::once(&point.volume_ratio))
                .chain(&point.affine_x)
                .chain(&point.affine_y)
                .chain(&point.affine_z)
                .all(|value| value.is_finite())
        }));
        assert_eq!(stats.nonfinite, 0);
        assert!(stats.live > 0);
        assert_eq!(
            stats.tick, 1,
            "two completed ticks have zero-based final index 1"
        );
        assert_eq!(
            scene.simulation_time().to_bits(),
            (((0.25 + 4.0) * TICK) as f32).to_bits()
        );
        (points, stats, scene.simulation_time())
    };
    let (grouped, grouped_stats, grouped_time) = run(true);
    let (separate, separate_stats, separate_time) = run(false);
    assert_eq!(grouped.len(), separate.len());
    assert!(
        bytemuck::cast_slice::<MatterPoint, u8>(&grouped)
            == bytemuck::cast_slice::<MatterPoint, u8>(&separate),
        "raw Matter points differ between grouped and separate export frames"
    );
    assert_eq!(grouped_stats, separate_stats);
    assert_eq!(grouped_time.to_bits(), separate_time.to_bits());
}

#[test]
fn matter_seed_changes_jitter() {
    let first = |seed: u32| {
        let mut scene = MatterScene::new(&SceneSettings {
            seed,
            ..small_dam_break()
        });
        scene.tick();
        scene.points()
    };
    let a = first(0);
    let b = first(1);
    assert_eq!(a.len(), b.len());
    let moved = a
        .iter()
        .zip(&b)
        .filter(|(p, q)| p.position != q.position)
        .count();
    assert!(
        moved > a.len() * 9 / 10,
        "only {moved} of {} points moved with the seed",
        a.len()
    );
}

/// Live (a preview budget in scope): a non-finite gravity holds the liquid
/// with a named error; recovery accepts at most two fixed intervals and
/// discards excess elapsed time, so the next frame owes only its own tick.
#[test]
fn matter_domain_holds_on_nonfinite_gravity() {
    let mut scene = MatterScene::new(&small_dam_break());
    scene
        .executor
        .set_sim_step(manifold_water_rigid::physics::SimStep::live(Seconds(
            1.0 / 60.0,
        )));
    for _ in 0..5 {
        scene.tick();
    }
    scene.set_domain("gravity", f32::NAN);
    scene.tick();
    let held = scene.points();
    assert_eq!(scene.state_input("ticks"), 0.0);
    scene.tick();
    assert_eq!(
        bytemuck::cast_slice::<MatterPoint, u8>(&held),
        bytemuck::cast_slice::<MatterPoint, u8>(&scene.points())
    );
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
    let mut scene = MatterScene::new(&SceneSettings {
        domain_size: 4.0,
        resolution: 64,
        fill_height: 0.2,
        ..SceneSettings::default()
    });
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 21.0);
    scene.set_material("stiffness", 0.5);
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 14.0);
    scene.set_material("stiffness", 2.0);
    scene.tick();
    assert_eq!(scene.state_input("substeps_per_tick"), 35.0);
    assert!(scene.frame_input("count") > 0.0);
}
