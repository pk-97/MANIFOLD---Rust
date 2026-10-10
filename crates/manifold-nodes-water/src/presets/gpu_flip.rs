//! Uses the PIC/FLIP ratio (0.95 FLIP) and the Dam Break scene values from FLIP Fluids fluidsimulation.h `_ratioPICFLIP` and the Dam Break preset (MIT); see THIRD_PARTY_NOTICES.md.
//! Checked against FLIP Fluids the engine's Dam Break scene (WaterDamBreak.json) (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP, the GPU water solver (docs/GPU_FLIP_PRESSURE_SOLVE.md), as
//! graphs built for any lattice. `water_def` is a running liquid on the
//! liquid seam (docs/LIQUID_SOLVER_SEAM_DESIGN.md P7a): node.gpu_flip_domain's clock runs
//! node.liquid_state's tick region, whose body is one Sim Rate tick of
//! one node.gpu_flip_step of [`STEPS_PER_TICK`] substeps, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakGpuFlip.json`, which is its own Dam Break at 64, and
//! `particle_view_def` draws that render's particles for the shipped
//! `WaterDamBreakParticles.json`. Every node reads the domain's lattice off
//! its wires, so a Resolution change reaches the running graph; the params
//! only seed the planned sizes.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, find_node};
#[cfg(any(test, feature = "testkit"))]
use manifold_core::effect_graph_def::find_node_mut;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use serde_json::{Value, json};

use manifold_water_gpu_flip::primitives::gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry};
use manifold_water_gpu_flip::primitives::gpu_flip_step::FACE_VALID_LAYERS;
use manifold_node_engine::load::catalog_source::preset_json as bundled_preset_json;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
use manifold_core::fluid_domain::{FluidDomainLayout, domain_layout};
use manifold_water_liquid::clock::INTERVAL_DURATION_INPUTS;
use manifold_water_liquid::grid::FACE_INPUT_PORTS;
use manifold_node_engine::scene::transform::Transform;

/// The FLIP Fluids engine's Dam Break tank side, the scenes' default Domain
/// Size; a scene's own `size` is what every measure reads.
const DAM_BREAK_METRES: f64 = 4.0;

/// Water substeps per liquid tick, the step node's Steps. A collider
/// moves per substep: each places it where its tick's row has it at the
/// substep's end.
pub(crate) const STEPS_PER_TICK: usize = 1;

manifold_core::testkit_visible! {
/// The main solve's iterations: the step's Auto.
pub(crate) const PRESSURE_ITERATIONS: usize = manifold_water_gpu_flip::primitives::gpu_flip_pressure::MAX_ITERATIONS as usize;
}

manifold_core::testkit_visible! {
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressureShape {
    /// Cells per side of the cubic lattice.
    pub n: usize,
    /// Conjugate gradient iterations, one V-cycle each.
    pub iterations: usize,
}
}

impl PressureShape {
    pub fn at(n: usize) -> Self {
        Self { n, iterations: PRESSURE_ITERATIONS }
    }
}

manifold_core::testkit_visible! {
/// The FLIP Fluids engine's Dam Break (`WaterDamBreak.json`): a 4 m tank
/// over the floor, a 0.16 m pool, and the `initial_column` block, seeded by
/// the engine's half-cell site rule.
pub(crate) const DAM_FILL_HEIGHT: f64 = 0.16;
}
manifold_core::testkit_visible! {
pub(crate) const DAM_COLUMN: [[f64; 2]; 3] = [[-1.84, -0.66], [0.16, 2.08], [-1.75, 1.75]];
}

manifold_core::testkit_visible! {
/// The Dam Break's box obstacle, the transform of `WaterDamBreak.json`'s
/// `obstacle_transform`: a unit cube scaled to 0.6 × 1.16 × 0.85 m standing
/// on the floor in the column's path. Position, then scale.
pub(crate) const DAM_OBSTACLE: [[f64; 3]; 2] = [[0.35, 0.58, -0.1], [0.6, 1.16, 0.85]];
}

/// Fluid role Collider and the role source's built-in cube, whose circumradius
/// 0.866 makes it a unit cube before the transform.
const COLLIDER_ROLE: usize = 3;
const CUBE_SHAPE: usize = 1;
const UNIT_CUBE_RADIUS: f64 = 0.866_025_4;

manifold_core::testkit_visible! {
/// A liquid in a cubic tank: a pool `fill_height` deep plus one box, both
/// in metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaterScene {
    pub pressure: PressureShape,
    /// The tank's side in metres: the domain's Domain Size.
    pub size: f64,
    /// Water substeps per tick: the step node's Steps.
    pub steps: usize,
    /// The FLIP share kept per step, the FLIP Fluids engine's 0.95.
    pub flip: f64,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// Surface lattice nodes per cell (`resolution_scale` of the surface's
    /// volume and mesh): Surface Detail 0 is subdivision 1, the FLIP Fluids
    /// engine default.
    pub surface_scale: usize,
    /// Publish the face grid: three node.face_sample_component named
    /// [`FACE_NODES`] on the state's faces after the region, into the frame.
    /// The tick always hands its last step's faces to the state.
    pub faces: bool,
    /// Place the free surface where the particles' distance crosses zero
    /// (ghost fluid). Off wires zero distances: air at zero pressure on its
    /// cell centres, the race's comparison.
    pub ghost_fluid: bool,
    /// Optional non-native density projection (Volume Projection). Off by
    /// default: matched-input motion is closer to native without its extra
    /// solve. Explicit saved opt-ins remain supported.
    pub volume_projection: bool,
    /// The Dam Break's box as a Collider role (`obstacle_transform` into
    /// `obstacle_collider` into the domain's `role_0`).
    pub obstacle: bool,
    /// The tank's closed faces, bit 2d the low face of axis d and bit 2d + 1
    /// the high one; an open face's Closed param is off on the domain.
    pub closed_faces: u32,
}
}

/// The domain's Closed params, in mask bit order.
const CLOSED_PARAMS: [&str; 6] = ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"];

manifold_core::testkit_visible! {
/// The face grid's nodes in a scene built with `faces`, x, y and z.
pub(crate) const FACE_NODES: [&str; 3] = ["face_u", "face_v", "face_w"];
}

manifold_core::testkit_visible! {
/// The water step node in every scene.
pub(crate) const STEP_NODE: &str = "step";
}

/// Particles per cell the fill seeds: one per half-cell site.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub const REST_PER_CELL: f64 = 8.0;

impl WaterScene {
    /// The engine's Dam Break, its box obstacle standing in the column's path.
    pub fn dam_break(n: usize) -> Self {
        Self {
            pressure: PressureShape::at(n),
            size: DAM_BREAK_METRES,
            steps: STEPS_PER_TICK,
            flip: 0.95,
            fill_height: DAM_FILL_HEIGHT,
            column: DAM_COLUMN,
            surface: false,
            surface_scale: 1,
            faces: false,
            ghost_fluid: true,
            volume_projection: false,
            obstacle: true,
            closed_faces: 63,
        }
    }

    /// The scene with only the faces in `mask` closed.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_closed_faces(self, mask: u32) -> Self {
        Self { closed_faces: mask, ..self }
    }

    /// The Dam Break the FLIP engine races: no obstacle, as `race_probe` and
    /// the race clips run the engine.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn race_dam_break(n: usize) -> Self {
        Self { obstacle: false, ..Self::dam_break(n) }
    }

    /// The scene with the Dam Break's box obstacle.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_obstacle(self) -> Self {
        Self { obstacle: true, ..self }
    }

    /// A pool 1 m deep and nothing else (I5). Every scene built from it
    /// leaves the box out unless it asks with [`Self::with_obstacle`].
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub fn still_pool(n: usize) -> Self {
        Self { fill_height: 1.0, column: [[0.0; 2]; 3], obstacle: false, ..Self::dam_break(n) }
    }

    /// A pool `fill` deep in a tank `size` on a side at `n` cells: the
    /// conformance box scenes' water.
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub fn pool(n: usize, size: f64, fill: f64) -> Self {
        Self { size, fill_height: fill, ..Self::still_pool(n) }
    }

    /// A pool 3 m deep in the 4 m tank: the coarsest multigrid level is
    /// water but for its top row, the hardest case for the coarse solve.
    #[cfg(any(test, feature = "testkit"))]
    pub fn deep_pool(n: usize) -> Self {
        Self { fill_height: 3.0, ..Self::still_pool(n) }
    }

    /// The deep pool with a 1 m × 0.5 m × 1 m block dropped in from 0.2 m
    /// above: a still pool's density source is zero, this one crowds.
    #[cfg(any(test, feature = "testkit"))]
    pub fn deep_drop(n: usize) -> Self {
        Self { column: [[-0.5, 0.5], [3.2, 3.7], [-0.5, 0.5]], ..Self::deep_pool(n) }
    }

    /// A 0.9 m pool with a 0.2 m slab over its left half: 1 m mean depth,
    /// a step in the surface whose sloshing is mostly the tank's first
    /// standing wave.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn slosh(n: usize) -> Self {
        Self { fill_height: 0.9, column: [[-2.0, 0.0], [0.9, 1.1], [-2.0, 2.0]], obstacle: false, ..Self::dam_break(n) }
    }

    /// A 1 m block of water high in the tank, clear of every wall.
    #[cfg(any(test, feature = "testkit"))]
    pub fn free_fall(n: usize) -> Self {
        Self { fill_height: 0.0, column: [[-0.5, 0.5], [2.5, 3.5], [-0.5, 0.5]], ..Self::still_pool(n) }
    }

    pub fn with_surface(self) -> Self {
        Self { surface: true, ..self }
    }

    /// Publish the face grid (section 3.2 (Grid outputs) of the seam).
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub fn with_faces(self) -> Self {
        Self { faces: true, ..self }
    }

    /// Meshed at `scale` surface nodes per cell.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_surface_scale(self, scale: usize) -> Self {
        Self { surface: true, surface_scale: scale, ..self }
    }

    /// The same scene with `iterations` per pressure solve.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_iterations(self, iterations: usize) -> Self {
        Self { pressure: PressureShape { iterations, ..self.pressure }, ..self }
    }

    /// The cell side in metres: the tank's side over the lattice.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn cell_size(&self) -> f64 {
        self.size / self.pressure.n as f64
    }

    /// `steps` water steps a frame.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_steps(self, steps: usize) -> Self {
        Self { steps, ..self }
    }

    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    /// The tank: the domain's layout at this resolution, no domain box.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn layout(&self) -> FluidDomainLayout {
        domain_layout(None, self.size as f32, self.pressure.n as u32).expect("the tank's layout")
    }

    /// The tank's lowest corner.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn min(&self) -> [f64; 3] {
        self.layout().min.map(f64::from)
    }

    /// The column as the domain's initial volume; none when it is empty.
    pub fn initial_volume(&self) -> Option<Transform> {
        let size = self.column.map(|[lo, hi]| hi - lo);
        (size.iter().all(|s| *s > 0.0)).then(|| Transform {
            pos: self.column.map(|[lo, hi]| (0.5 * (lo + hi)) as f32),
            scale: size.map(|s| s as f32),
            ..Transform::default()
        })
    }

    /// The domain's own reading of the scene: the fill's sites, the padded
    /// lattice and the particle count, from node.gpu_flip_domain's function.
    pub fn geometry(&self) -> GpuFlipGeometry {
        let n = self.pressure.n as f32;
        let read = |name: &str, default: f32| match name {
            "resolution" => n,
            "domain_size" => self.size as f32,
            "fill_height" => self.fill_height as f32,
            _ => default,
        };
        gpu_flip_geometry(read, None, self.initial_volume()).expect("the scene fits its domain")
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn pool_sites(&self) -> u32 {
        self.geometry().setup.pool_sites
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn box_sites(&self) -> [[u32; 2]; 3] {
        self.geometry().setup.box_sites
    }

    /// Particles the fill places; the particle arrays hold exactly this many.
    #[cfg(any(test, feature = "testkit"))]
    pub fn particles(&self) -> u64 {
        self.geometry().particles
    }
}

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn int(v: usize) -> Value {
    json!({"type": "Int", "value": v})
}

#[derive(Default)]
struct Builder {
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
}

type Port = (usize, &'static str);

impl Builder {
    fn node(&mut self, name: &str, type_id: &str, params: Value) -> usize {
        let id = self.nodes.iter().map(|n| n.id as usize + 1).max().unwrap_or(0);
        self.nodes.push(serde_json::from_value(json!({"id": id, "nodeId": name, "typeId": type_id, "params": params})).expect("recipe node"));
        id
    }

    fn wire(&mut self, from: (usize, &str), to: usize, port: &str) {
        self.wires.push(EffectGraphWire {
            from_node: from.0 as u32, from_port: from.1.into(), to_node: to as u32, to_port: port.into(),
        });
    }

    /// The same-named scalar outputs of `from` into `to`.
    fn wires(&mut self, from: usize, to: usize, ports: &[&'static str]) {
        for &port in ports {
            self.wire((from, port), to, port);
        }
    }

    fn from_def(def: EffectGraphDef) -> Self {
        Self { nodes: def.nodes, wires: def.wires }
    }

    /// Look up a node in this recipe scope; callers enter a group explicitly.
    fn id(&self, name: &str) -> usize {
        self.nodes.iter().find(|n| n.node_id.as_str() == name)
            .unwrap_or_else(|| panic!("recipe scope has no {name}")).id as usize
    }

    fn remove(&mut self, names: &[&str]) {
        let ids: Vec<_> = self.nodes.iter().filter(|n| names.contains(&n.node_id.as_str())).map(|n| n.id).collect();
        self.nodes.retain(|n| !ids.contains(&n.id));
        self.wires.retain(|w| !ids.contains(&w.from_node) && !ids.contains(&w.to_node));
    }

    fn finish(self) -> EffectGraphDef {
        serde_json::from_value(json!({"version": 3, "nodes": self.nodes, "wires": self.wires})).expect("recipe graph")
    }
}

/// Feeds every interval input ([`INTERVAL_DURATION_INPUTS`]) among `nodes`
/// that no wire feeds yet from `domain`'s accepted interval.
fn feed_intervals(nodes: &[EffectGraphNode], wires: &mut Vec<EffectGraphWire>, domain: u32) {
    for node in nodes {
        for &(type_id, port) in &INTERVAL_DURATION_INPUTS {
            if node.type_id != type_id || wires.iter().any(|w| w.to_node == node.id && w.to_port == port) {
                continue;
            }
            wires.push(EffectGraphWire { from_node: domain, from_port: "interval_duration".into(), to_node: node.id, to_port: port.into() });
        }
    }
}

/// The padded lattice's scalars, as the domain publishes them.
const LATTICE_WIRES: [&str; 7] = ["lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z"];

/// The fill's sites, as the domain publishes them.
const FILL_WIRES: [&str; 7] = ["pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1"];

/// The scene's forces and impulses, as the domain publishes them.
const FIELD_WIRES: [&str; 9] = [
    "forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices",
    "first_tick", "impulse_tick",
];

manifold_core::testkit_visible! {
/// A scene as a running liquid on the seam. The domain seeds the fill and
/// runs the clock; the state's region runs one tick per due tick: `steps`
/// water steps from `state.out`, then the tick's stats, closing into
/// `state.in` and `state.stats_in`. The frame publishes each tick with the
/// solid lattice. The harness sink holds the frame, or the surface mesh when
/// `surface`.
pub(crate) fn water_def(scene: WaterScene) -> EffectGraphDef {
    let mut b = Builder::default();
    let geometry = scene.geometry();
    let mut params = json!({
        "resolution": int(scene.pressure.n),
        "domain_size": float(scene.size),
        "fill_height": float(scene.fill_height),
    });
    for (bit, name) in CLOSED_PARAMS.iter().enumerate() {
        if scene.closed_faces & (1 << bit) == 0 {
            params[*name] = json!({"type": "Bool", "value": false});
        }
    }
    let domain = b.node("domain", GPU_FLIP_DOMAIN_TYPE_ID, params);
    if let Some(volume) = scene.initial_volume() {
        let column = b.node(
            "initial_column",
            "node.transform_3d",
            json!({
                "pos_x": float(f64::from(volume.pos[0])),
                "pos_y": float(f64::from(volume.pos[1])),
                "pos_z": float(f64::from(volume.pos[2])),
                "scale_x": float(f64::from(volume.scale[0])),
                "scale_y": float(f64::from(volume.scale[1])),
                "scale_z": float(f64::from(volume.scale[2])),
            }),
        );
        b.wire((column, "transform"), domain, "initial_volume");
    }
    if scene.obstacle {
        let (_, collider) = obstacle_source(&mut b);
        b.wire((collider, "role"), domain, "role_0");
    }
    // The params hold the domain's own sites, so the planned storage is the
    // fill's; the wires carry any change.
    let sites = geometry.setup.box_sites;
    let fill = b.node(
        "fill",
        "node.liquid_fill",
        padded_lattice(
            &scene,
            &[
                ("pool_sites", int(geometry.setup.pool_sites as usize)),
                ("box_x0", int(sites[0][0] as usize)),
                ("box_x1", int(sites[0][1] as usize)),
                ("box_y0", int(sites[1][0] as usize)),
                ("box_y1", int(sites[1][1] as usize)),
                ("box_z0", int(sites[2][0] as usize)),
                ("box_z1", int(sites[2][1] as usize)),
                ("jitter", float(0.0)),
                ("seed", int(0)),
            ],
        ),
    );
    b.wires(domain, fill, &FILL_WIRES);
    b.wires(domain, fill, &LATTICE_WIRES);
    b.wires(domain, fill, &["bodies", "shapes", "atlas", "body_count", "epoch", "particle_capacity"]);
    b.wire((domain, "mesh_wall_inset"), fill, "wall_inset");
    let count = (fill, "count");
    let state = b.node("state", "node.liquid_state", json!({}));
    b.wire((fill, "particles"), state, "seed");
    b.wire(count, state, "count");
    b.wires(domain, state, &["ticks", "epoch", "simulation_time", "target_time", "dropped_seconds"]);
    let particles: Port = (state, "out");
    let step = water_step(&mut b, scene, (domain, state));
    b.wire(particles, step, "particles");
    b.wire((state, "identity"), step, "identity");
    b.wire((step, "identity_out"), state, "identity_in");
    b.wire(count, step, "count");
    b.wire((domain, "reaction"), step, "reaction");
    b.wires(domain, step, &["regions", "region_count", "epoch"]);
    let (particles, faces) = ((step, "out"), (step, "faces"));
    let stats = b.node("stats", "node.liquid_stats", json!({}));
    b.wire(particles, stats, "particles");
    b.wire((state, "stats"), stats, "stats");
    b.wire(count, stats, "count");
    b.wire((domain, "particle_mass"), stats, "particle_mass");
    b.wire((step, "capped"), stats, "capped");
    b.wire(particles, state, "in");
    b.wire((stats, "stats_out"), state, "stats_in");
    // The tick's last faces leave the region beside its particles, into the
    // lattice's face grid.
    b.wire(faces, state, "faces_in");
    b.wire((step, "interior"), state, "interior_in");
    b.wires(domain, state, &["nodes_x", "nodes_y", "nodes_z"]);

    // Solver faces, published surface and whitewater share the native grid.
    const NATIVE_GRID: [(&str, &str); 6] = [
        ("mesh_min_x", "lattice_min_x"), ("mesh_min_y", "lattice_min_y"), ("mesh_min_z", "lattice_min_z"),
        ("mesh_nodes_x", "nodes_x"), ("mesh_nodes_y", "nodes_y"), ("mesh_nodes_z", "nodes_z"),
    ];
    let solid = b.node("mesh_solid", "node.liquid_solid_distance", json!({}));
    let source = b.node("whitewater_obstacle_source", "node.whitewater_obstacle_source", json!({}));
    for node in [solid, source] {
        b.wires(domain, node, &["bodies", "shapes", "atlas", "closed_faces", "body_count"]);
        b.wire((domain, "cell_size"), node, "cell_size");
        for (from, to) in NATIVE_GRID {
            b.wire((domain, from), node, to);
        }
        b.wire((domain, "body_rows"), node, "rows");
        b.wire((domain, "mesh_wall_inset"), node, "wall_inset");
    }
    let frame = b.node("frame", "node.liquid_frame", json!({"face_valid_layers": int(FACE_VALID_LAYERS as usize)}));
    b.wire((state, "out"), frame, "particles");
    b.wire((state, "stats"), frame, "stats");
    b.wire((state, "identity"), frame, "identity");
    b.wire((state, "interior"), frame, "interior");
    b.wire((solid, "solid"), frame, "solid");
    b.wire(count, frame, "count");
    b.wires(domain, frame, &LATTICE_WIRES);
    b.wires(domain, frame, &["closed_faces", "simulation_time", "display_time", "epoch", "display_cursor", "dropped_seconds"]);
    if scene.faces {
        let nodes = scene.geometry().setup.lattice.nodes();
        for (axis, name) in FACE_NODES.into_iter().enumerate() {
            let params = json!({
                "axis": {"type": "Enum", "value": axis},
                "nodes_x": float(f64::from(nodes[0])),
                "nodes_y": float(f64::from(nodes[1])),
                "nodes_z": float(f64::from(nodes[2])),
            });
            let id = b.node(name, "node.face_sample_component", params);
            b.wire((state, "faces"), id, "faces");
            b.wires(domain, id, &["nodes_x", "nodes_y", "nodes_z"]);
            b.wire((id, "out"), frame, FACE_INPUT_PORTS[axis]);
        }
    }
    // Solids pose their bodies at the end of the accepted interval, so they
    // agree with the step at every Sim Rate.
    feed_intervals(&b.nodes, &mut b.wires, domain as u32);

    let output = b.node("output", "system.final_output", json!({}));
    let sink = if scene.surface {
        let mesh = surface(&mut b, scene, frame);
        let sink = b.node("mesh_sink", "test.mesh_sink", json!({}));
        b.wire(mesh, sink, "vertices");
        sink
    } else {
        let sink = b.node("sink", "test.liquid_sink", json!({}));
        b.wire((frame, "particles_b"), sink, "particles");
        sink
    };
    b.wire((sink, "out"), output, "in");
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("water def")
}
}

/// The collider and its visible box share this one transform in the preset.
fn obstacle_source(b: &mut Builder) -> (usize, usize) {
    let [pos, scale] = DAM_OBSTACLE;
    let transform = b.node(
        "obstacle_transform",
        "node.transform_3d",
        json!({
            "pos_x": float(pos[0]), "pos_y": float(pos[1]), "pos_z": float(pos[2]),
            "scale_x": float(scale[0]), "scale_y": float(scale[1]), "scale_z": float(scale[2]),
        }),
    );
    let collider = b.node(
        "obstacle_collider",
        "node.fluid_role_source",
        json!({
            "role": {"type": "Enum", "value": COLLIDER_ROLE},
            "shape": {"type": "Enum", "value": CUBE_SHAPE},
            "radius": float(UNIT_CUBE_RADIUS),
        }),
    );
    b.wire((transform, "transform"), collider, "transform");
    (transform, collider)
}

manifold_core::testkit_visible! {
/// The shipped GPU FLIP Dam Break supplies the authored camera, lights,
/// environment, tank and tone map, plus the shared Liquid Surface group.
/// The family itself is constructed by the recipe below.
pub(crate) const SHIPPED_PRESET: &str = "WaterDamBreakGpuFlip";
}

fn shipped_preset() -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(SHIPPED_PRESET)).expect("the GPU FLIP preset is bundled");
    serde_json::from_str(&json).expect("the GPU FLIP preset parses")
}

/// Reuse the existing nested surface verbatim.
fn surface_group() -> Value {
    let preset: EffectGraphDef = serde_json::from_value(shipped_preset()).expect("preset");
    serde_json::to_value(find_node(&preset.nodes, "surface").expect("liquid surface group"))
        .expect("surface group")
}

/// Stage and capture populations, including the unrendered dust oracle.
pub(super) const WHITEWATER_KINDS: [&str; 4] = ["foam", "bubble", "spray", "dust"];
/// Child looks in family-output order. Dust has no display chain.
const RENDERED_KINDS: [&str; 3] = ["foam", "spray", "bubble"];
const FAMILY_OUTPUTS: [&str; 4] = ["object", "object_1", "object_2", "object_3"];

/// The body output retained by the insertion template.
pub const LIQUID_BODY_OUTPUT: &str = "fluid_output";

impl WaterScene {
    /// The shared insertion body: simulation, captured display and four looks.
    /// Preset colliders enter through the ordinary domain role interface.
    fn family_def(self) -> EffectGraphDef {
        let mut b = Builder::from_def(water_def(Self { obstacle: false, ..self.with_surface() }));
        b.remove(&["mesh_sink", "output"]);
        let [domain, state, step, frame, solid, source, surface] =
            ["domain", "state", STEP_NODE, "frame", "mesh_solid", "whitewater_obstacle_source", "surface"].map(|n| b.id(n));
        if self.obstacle {
            let input = b.node("fluid_input", "system.group_input", json!({}));
            b.wire((input, "role_0"), domain, "role_0");
        }
        let whitewater = b.node("whitewater", "node.whitewater_step", json!({
            "capacity": int(100_000), "wavecrest_emission": float(175.0),
            "min_energy": float(0.1), "max_energy": float(60.0), "amount": float(1.0),
        }));
        let budget = b.node("whitewater_budget", "node.value", json!({"value": float(100_000.0)}));
        b.wire((budget, "out"), whitewater, "capacity");
        b.wire((budget, "out"), state, "whitewater_capacity");
        b.wires(step, whitewater, &["grid_bounds", "grid_nodes_x", "grid_nodes_y", "grid_nodes_z",
            "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers", "distance", "faces",
            "substep_schedule", "substep_u", "substep_v", "substep_w", "substep_count"]);
        b.wire((step, "out"), whitewater, "particles");
        b.wire((solid, "solid"), whitewater, "solid");
        b.wire((source, "solid"), whitewater, "obstacle_source");
        b.wires(domain, whitewater, &["epoch", "gravity_x", "gravity", "gravity_z",
            "forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z",
            "field_spacing", "force_lattices", "impulse_tick", "first_tick", "regions",
            "region_count", "shapes", "atlas"]);
        b.wire((state, "tick_index"), whitewater, "tick_index");
        for (held, input) in [("whitewater_pool", "pool"), ("whitewater_state", "pool_state")] {
            b.wire((state, held), whitewater, input);
        }
        for (output, capture) in [("pool_out", "whitewater_pool_in"),
            ("state_out", "whitewater_state_in"), ("counts_out", "whitewater_counts_in")] {
            b.wire((whitewater, output), state, capture);
        }
        for kind in WHITEWATER_KINDS {
            b.wire((whitewater, &format!("{kind}_particles")), state, &format!("{kind}_particles_in"));
            // All four captures remain observable; only three have rendered looks.
            b.wire((state, &format!("{kind}_particles")), frame, &format!("{kind}_in"));
        }

        let particles = b.node("particle_blend", "node.interpolate_particle_frames", json!({}));
        let blend = b.node("solid_blend", "node.mix_arrays", json!({}));
        let push_out = b.node("particle_push_out", "node.push_out_of_solid", json!({}));
        let lattice_box = b.node("display_lattice_box", "node.transform_components", json!({}));
        b.wires(frame, particles, &["particles_a", "particles_b", "count_a", "count_b", "identity_a", "identity_b", "blend", "span"]);
        b.wire((frame, "solid_a"), blend, "a");
        b.wire((frame, "solid_b"), blend, "b");
        b.wire((frame, "blend"), blend, "amount");
        b.wire((particles, "out"), push_out, "particles");
        b.wire((blend, "out"), push_out, "solid");
        b.wire((frame, "grid_bounds"), lattice_box, "transform");
        for axis in ["x", "y", "z"] {
            b.wire((lattice_box, &format!("pos_{axis}")), push_out, &format!("center_{axis}"));
            b.wire((lattice_box, &format!("scale_{axis}")), push_out, &format!("size_{axis}"));
            b.wire((frame, &format!("grid_nodes_{axis}")), push_out, &format!("nodes_{axis}"));
        }
        b.wires.retain(|w| w.to_node != surface as u32 || !["particles", "solid"].contains(&w.to_port.as_str()));
        b.wire((push_out, "out"), surface, "particles");
        b.wire((blend, "out"), surface, "solid");

        let material = b.node("water_material", "node.pbr_material", json!({
            "ambient": float(0.0), "color_r": float(0.37254903), "color_g": float(0.7294118), "color_b": float(1.0),
            "metallic": float(0.0), "roughness": float(0.5540391), "ior": float(1.333), "transmission": float(1.0),
            "volume_attenuation_color_r": float(0.35), "volume_attenuation_color_g": float(0.72), "volume_attenuation_color_b": float(0.8),
            "volume_attenuation_distance": float(0.1), "volume_geometry": float(1.0), "volume_thickness": float(0.09),
            "volume_scattering_color_r": float(0.68), "volume_scattering_color_g": float(0.86), "volume_scattering_color_b": float(0.92),
            "volume_scattering_density": float(0.08826533),
        }));
        let water = b.node("water_object", "node.scene_object", json!({}));
        b.wire((surface, "vertices"), water, "vertices");
        b.wire((surface, "indices"), water, "indices");
        b.wire((material, "out"), water, "material");
        let output = b.node(LIQUID_BODY_OUTPUT, "system.group_output", json!({}));
        b.wire((water, "object"), output, FAMILY_OUTPUTS[0]);
        for (index, kind) in RENDERED_KINDS.into_iter().enumerate() {
            let (radius, roughness, transmission) = match kind {
                "foam" => (0.005, 0.55, 0.0),
                "spray" => (0.003, 0.12, 1.0),
                "bubble" => (0.0035, 0.3, 0.35),
                _ => unreachable!("rendered kind"),
            };
            let mesh = b.node(&format!("{kind}_mesh"), "node.platonic_solid_mesh",
                json!({"radius": float(radius), "shape": {"type": "Enum", "value": 3}}));
            let mut params = json!({
                "color_r": float(0.95), "color_g": float(0.98), "color_b": float(1.0), "ior": float(1.333),
                "metallic": float(0.0), "roughness": float(roughness), "transmission": float(transmission),
                "volume_thickness": float(radius * 2.0),
            });
            if kind == "bubble" { params["volume_particle_density"] = float(70.0); }
            let material = b.node(&format!("{kind}_material"), "node.pbr_material", params);
            let object = b.node(&format!("{kind}_object"), "node.scene_object", json!({"cast_shadows": float(0.0)}));
            let copies = b.node(&format!("{kind}_copies"), "node.particles_to_copies", json!({}));
            let display = b.node(&format!("{kind}_blend"), "node.interpolate_particle_frames", json!({}));
            b.wire((frame, &format!("{kind}_b")), display, "particles_b");
            b.wires(frame, display, &["blend", "span"]);
            b.wire((display, "out"), copies, "particles");
            b.wire((copies, "copies"), object, "instances");
            b.wire((mesh, "vertices"), object, "vertices");
            b.wire((material, "out"), object, "material");
            b.wire((object, "object"), output, FAMILY_OUTPUTS[index + 1]);
            if kind == "spray" {
                for (from, to) in [("gravity_x", "acceleration_x"), ("gravity", "acceleration_y"), ("gravity_z", "acceleration_z")] {
                    b.wire((domain, from), display, to);
                }
            }
        }
        // Resolve durations inside the domain-owning scope before it is wrapped.
        feed_intervals(&b.nodes, &mut b.wires, domain as u32);
        for node in &mut b.nodes {
            if node.group.is_some() { continue; }
            node.handle = match node.node_id.as_str() {
                "water_material" => Some("Water Material"),
                "water_object" => Some("Water"),
                "whitewater" => Some("Whitewater"),
                "whitewater_budget" => Some("Whitewater Budget"),
                "foam_object" => Some("Foam"),
                "spray_object" => Some("Spray"),
                "bubble_object" => Some("Bubbles"),
                "foam_mesh" => Some("Foam Mesh"),
                "foam_material" => Some("Foam Material"),
                "foam_copies" => Some("Foam Copies"),
                "spray_mesh" => Some("Spray Mesh"),
                "spray_material" => Some("Spray Material"),
                "spray_copies" => Some("Spray Copies"),
                "bubble_mesh" => Some("Bubble Mesh"),
                "bubble_material" => Some("Bubble Material"),
                "bubble_copies" => Some("Bubble Copies"),
                _ => None,
            }.map(str::to_owned);
        }
        let mut def = b.finish();
        // Author the parent gate once. Insertion remaps this metadata and
        // presets copy it intact; neither consumer recreates the target list.
        let mut bindings: Vec<_> = FAMILY_OUTPUTS.iter().map(|port| {
            let wire = def.wires.iter().find(|wire| wire.to_node == output as u32 && wire.to_port == *port)
                .expect("family object output");
            let object = def.nodes.iter().find(|node| node.id == wire.from_node).expect("family object");
            json!({"id":"parent_visible", "label":"Visible", "defaultValue":1.0,
                "convert":{"type":"Float"},
                "target":{"kind":"node", "nodeId":object.node_id, "param":"parent_visible"}})
        }).collect();
        let budget_node = def.nodes.iter().find(|node| node.id == budget as u32).expect("family budget");
        let domain_node = def.nodes.iter().find(|node| node.id == domain as u32).expect("family domain");
        bindings.push(json!({"id":"sheet_fill_rate", "label":"Sheet Fill Rate", "defaultValue":0.0,
            "defaultMirrorsNodeParam":true, "convert":{"type":"Float"},
            "target":{"kind":"node", "nodeId":domain_node.node_id, "param":"sheet_fill_rate"}}));
        bindings.push(json!({"id":"whitewater_capacity", "label":"Whitewater Budget", "defaultValue":100000.0,
            "defaultMirrorsNodeParam":true, "convert":{"type":"IntRound"},
            "target":{"kind":"node", "nodeId":budget_node.node_id, "param":"value"}}));
        def.preset_metadata = Some(serde_json::from_value(json!({
            "id":"WaterFamily", "displayName":"Water", "category":"Geometry", "oscPrefix":"water",
            "params":[{"id":"parent_visible", "name":"Visible", "defaultValue":1.0,
                "min":0.0, "max":1.0, "isToggle":true, "cardVisible":false, "section":"Water"},
                {"id":"sheet_fill_rate", "name":"Sheet Fill Rate", "defaultValue":0.0,
                "min":0.0, "max":1.0, "formatString":"F2", "wholeNumbers":false,
                "isToggle":false, "isTrigger":false, "section":"Fluid"},
                {"id":"whitewater_capacity", "name":"Whitewater Budget", "defaultValue":100000.0,
                "min":1000.0, "max":250000.0, "wholeNumbers":true, "formatString":"F0", "section":"Water Detail"}],
            "bindings":bindings
        })).expect("family visibility metadata"));
        let metadata = def.preset_metadata.as_mut().expect("family metadata");
        let look_metadata = manifold_node_engine::scene::exposure_source::look_metadata();
        let visible_metadata: Vec<_> = manifold_node_engine::scene::exposure_source::metadata_for_node_type("node.scene_object")
            .into_iter().filter(|param| param.name == "visible").collect();
        for (kind, section) in [("foam", "Foam"), ("spray", "Spray"), ("bubble", "Bubbles")] {
            for (suffix, param_id, descriptors) in [
                ("mesh", format!("{kind}_size"), &look_metadata),
                ("object", format!("{kind}_visible"), &visible_metadata),
            ] {
                let node = def.nodes.iter().find(|node| node.node_id.as_str() == format!("{kind}_{suffix}"))
                    .expect("family look node");
                let mut params = Vec::new();
                let mut bindings = Vec::new();
                manifold_core::scene_exposure::stamp_scene_node_exposures_into(
                    &mut params, &mut bindings, node.id, &node.node_id, &node.type_id,
                    section, descriptors, &node.params,
                );
                assert_eq!(params.len(), 1, "one authored look control");
                for param in &mut params { param.id = param_id.clone(); }
                for binding in &mut bindings { binding.id = param_id.clone(); }
                metadata.params.extend(params);
                metadata.bindings.extend(bindings);
            }
        }
        def
    }
}

/// The insertion body is the same family used by both presets, without a collider.
pub fn gpu_flip_liquid_body() -> EffectGraphDef {
    let mut body = WaterScene { obstacle: false, ..WaterScene::dam_break(64) }.family_def();
    for (id, handle) in [("water_object", ""), ("domain", "Simulation"), ("initial_column", "Initial Volume")] {
        body.nodes.iter_mut().find(|node| node.node_id.as_str() == id)
            .expect("insertion body node").handle = Some(handle.into());
    }
    body
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const SURFACE_DETAIL_OFFSET: usize = 1;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const DRESSING: &[&str] = &["input", "camera", "environment", "light", "floor_mesh", "basin_material",
        "floor_transform", "floor_object", "rear_mesh", "rear_transform", "rear_object", "left_mesh",
        "left_transform", "left_object", "right_mesh", "right_transform", "right_object", "scene",
        "final_output", "rim_light", "filmic_display", "sky_environment", "sky_exposure", "environment_select"];

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn assert_preset_root(nodes: &[EffectGraphNode]) {
    for node in nodes {
        assert!(DRESSING.contains(&node.node_id.as_str()) || matches!(node.node_id.as_str(),
            "water_family" | "obstacle_transform" | "obstacle_collider" | "obstacle_mesh" |
            "obstacle_material" | "obstacle_object"),
            "unclassified top-level preset node would be discarded: {}", node.node_id.as_str());
    }
}

/// Only authored environment/camera/tank dressing is read from the preset.
/// The family is always constructed by WaterScene::family_def.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub fn render_def(scene: WaterScene) -> EffectGraphDef {
    let preset = shipped_preset();
    let mut def: EffectGraphDef = serde_json::from_value(preset.clone()).expect("preset");
    assert_preset_root(&def.nodes);
    def.nodes.retain(|n| DRESSING.contains(&n.node_id.as_str()));
    let ids: Vec<_> = def.nodes.iter().map(|n| n.id).collect();
    def.wires.retain(|w| ids.contains(&w.from_node) && ids.contains(&w.to_node));
    let mut b = Builder::from_def(def.clone());
    let render = b.id("scene");
    let body = scene.family_def();
    let family_metadata = body.preset_metadata.clone().expect("family metadata");
    let family = b.node("water_family", "group", json!({}));
    let inputs = if scene.obstacle { vec![json!({"name": "role_0", "portType": "FluidRole"})] } else { vec![] };
    let outputs: Vec<_> = FAMILY_OUTPUTS.iter().map(|name| json!({"name": name, "portType": "SceneObject"})).collect();
    let node = b.nodes.last_mut().expect("family");
    node.handle = Some("Water".into());
    node.group = Some(serde_json::from_value(json!({
        "interface": {"inputs": inputs, "outputs": outputs},
        "nodes": body.nodes, "wires": body.wires,
    })).expect("family group"));
    // Preserve the existing physical render order: water, foam, spray, bubbles.
    for (output, slot) in FAMILY_OUTPUTS.into_iter().zip(["object_0", "object_7", "object_9", "object_8"]) {
        b.wire((family, output), render, slot);
    }
    if scene.obstacle { add_obstacle_render(&mut b, family, render); }
    let render_node = b.nodes.iter_mut().find(|n| n.id == render as u32).expect("render");
    // Physical slots span 0 through 9; slot 6 is intentionally empty.
    render_node.params.insert("objects".into(), serde_json::from_value(int(10)).expect("object count"));
    def.nodes = b.nodes;
    def.wires = b.wires;
    let mut metadata: manifold_core::effect_graph_def::PresetMetadata =
        serde_json::from_value(scene_cards(&preset["presetMetadata"], scene)).expect("scene cards");
    for spec in family_metadata.params {
        let bindings: Vec<_> = family_metadata.bindings.iter()
            .filter(|binding| binding.id == spec.id).cloned().collect();
        let insertion = metadata.bindings.iter().position(|binding| binding.id == spec.id)
            .unwrap_or(metadata.bindings.len());
        metadata.bindings.retain(|binding| binding.id != spec.id);
        metadata.bindings.splice(insertion..insertion, bindings);
        if let Some(existing) = metadata.params.iter_mut().find(|existing| existing.id == spec.id) {
            *existing = spec;
        } else {
            metadata.params.push(spec);
        }
    }
    def.preset_metadata = Some(metadata);
    def
}

/// The obstacle remains an external scene object; its one transform drives
/// both the visible box and the collider role entering the family.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn add_obstacle_render(b: &mut Builder, family: usize, render: usize) {
    let (transform, collider) = obstacle_source(b);
    b.wire((collider, "role"), family, "role_0");
    let mesh = b.node("obstacle_mesh", "node.cube_mesh", json!({}));
    let material = b.node("obstacle_material", "node.pbr_material", json!({
        "color_r": float(0.52), "color_g": float(0.23), "color_b": float(0.073),
        "metallic": float(0.94), "roughness": float(0.19), "ambient": float(0.0),
    }));
    let object = b.node("obstacle_object", "node.scene_object", json!({}));
    b.wire((mesh, "vertices"), object, "vertices");
    b.wire((material, "out"), object, "material");
    b.wire((transform, "transform"), object, "transform");
    b.wire((object, "object"), render, "object_1");
    for (id, handle) in [(mesh, "Obstacle Mesh"), (material, "Obstacle Material"), (object, "Obstacle")] {
        b.nodes.iter_mut().find(|node| node.id == id as u32).expect("obstacle node")
            .handle = Some(handle.into());
    }
}

/// The shipped Particle View, which [`particle_view_def`] builds from the
/// shipped Dam Break (`gpu_flip_particle_view_is_built_from_the_dam_break`).
#[cfg(any(test, feature = "testkit"))]
pub const PARTICLE_VIEW_PRESET: &str = "WaterDamBreakParticles";

#[cfg(any(test, feature = "testkit"))]
const PARTICLE_VIEW_NAME: &str = "Water — Dam Break (Particle View)";

#[cfg(any(test, feature = "testkit"))]
const PARTICLE_VIEW_DESCRIPTION: &str = "GPU FLIP dam break drawn as small sphere copies of the blended, solid-clamped liquid particles, with the same display-time whitewater as the surface preset.";

/// The Platonic shape the Particle View draws each particle with, scaled by
/// the particle's radius.
#[cfg(any(test, feature = "testkit"))]
const ICOSAHEDRON: usize = 3;

/// The Particle View: the shipped Dam Break's render, simulation, scene,
/// whitewater, with the Liquid Surface replaced by
/// `particle_push_out`'s particles drawn as instanced spheres by the water
/// object. Its cards are the Dam Break's, less those that only reached the
/// surface.
#[cfg(any(test, feature = "testkit"))]
pub fn particle_view_def() -> EffectGraphDef {
    let mut def = render_def(WaterScene::dam_break(64));
    let family = find_node_mut(&mut def.nodes, "water_family").expect("Water family");
    let group = family.group.as_mut().expect("family group");
    let mut b = Builder { nodes: std::mem::take(&mut group.nodes), wires: std::mem::take(&mut group.wires) };
    b.remove(&["surface"]);
    let copies = b.node("liquid_particle_copies", "node.particles_to_copies", json!({}));
    let sphere = b.node("liquid_particle_mesh", "node.platonic_solid_mesh",
        json!({"radius": float(1.0), "shape": {"type": "Enum", "value": ICOSAHEDRON}}));
    let [frame, particles, water] = ["frame", "particle_push_out", "water_object"].map(|name| b.id(name));
    for (from, to) in [
        ((particles, "out"), (copies, "particles")),
        ((frame, "count_b"), (copies, "live_count")),
        ((copies, "copies"), (water, "instances")),
        ((sphere, "vertices"), (water, "vertices")),
        ((frame, "count_b"), (water, "instance_count")),
    ] {
        b.wire(from, to.0, to.1);
    }
    group.nodes = b.nodes;
    group.wires = b.wires;
    def.name = Some(PARTICLE_VIEW_NAME.into());
    def.description = Some(PARTICLE_VIEW_DESCRIPTION.into());
    let value = serde_json::to_value(&def).expect("particle view");
    def.preset_metadata = Some(serde_json::from_value(particle_view_cards(&value)).expect("particle cards"));
    def
}

/// `def`'s cards under the Particle View's id, less every binding whose node
/// is gone and every card left with none of the bindings it had.
#[cfg(any(test, feature = "testkit"))]
fn particle_view_cards(def: &Value) -> Value {
    fn names(nodes: &Value, into: &mut Vec<String>) {
        for node in nodes.as_array().into_iter().flatten() {
            into.extend(node["nodeId"].as_str().map(str::to_owned));
            names(&node["group"]["nodes"], into);
        }
    }
    let mut present = Vec::new();
    names(&def["nodes"], &mut present);
    let mut metadata = def["presetMetadata"].clone();
    for (bindings, cards) in [("bindings", "params"), ("stringBindings", "stringParams")] {
        let Some(list) = metadata[bindings].as_array_mut() else { continue };
        let bound = |list: &[Value]| list.iter().filter_map(|b| b["id"].as_str().map(str::to_owned)).collect::<Vec<_>>();
        let before = bound(list.as_slice());
        list.retain(|b| b["target"]["kind"] != "node" || b["target"]["nodeId"].as_str().is_some_and(|node| present.iter().any(|name| name == node)));
        let after = bound(list.as_slice());
        if let Some(cards) = metadata[cards].as_array_mut() {
            cards.retain(|card| card["id"].as_str().is_none_or(|id| !before.iter().any(|b| b == id) || after.iter().any(|a| a == id)));
        }
    }
    metadata["id"] = json!(PARTICLE_VIEW_PRESET);
    metadata["displayName"] = json!(PARTICLE_VIEW_NAME);
    metadata["oscPrefix"] = json!(PARTICLE_VIEW_PRESET.to_lowercase());
    metadata
}

/// The shipped cards with every default at the value this scene's def bakes,
/// so no card overwrites what the extent proof checked: Resolution at the
/// lattice, Surface Detail at the surface scale, gone past its range.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn scene_cards(metadata: &Value, scene: WaterScene) -> Value {
    let mut metadata = metadata.clone();
    let detail = scene.surface_scale.checked_sub(SURFACE_DETAIL_OFFSET).filter(|detail| *detail <= 2);
    for list in ["params", "bindings"] {
        let entries = metadata[list].as_array_mut().expect("card lists");
        entries.retain(|entry| entry["id"] != "surface_detail" || detail.is_some());
        for entry in entries.iter_mut() {
            if entry["id"] == "resolution" {
                entry["defaultValue"] = json!(scene.pressure.n as f64);
            }
            if let Some(detail) = detail
                && entry["id"] == "surface_detail"
            {
                entry["defaultValue"] = json!(detail as f64);
            }
        }
    }
    metadata
}

/// Sets `resolution_scale` on the surface schedule, volume and mesh in `value`,
/// however deep the group nests them.
fn set_surface_scale(value: &mut Value, scale: usize) {
    match value {
        Value::Object(map) => {
            let surface = map.get("nodeId").is_some_and(|id| id == "liquid_volume" || id == "liquid_mesh" || id == "liquid_bricks");
            if surface && let Some(params) = map.get_mut("params") {
                params["resolution_scale"] = int(scale);
            }
            map.values_mut().for_each(|v| set_surface_scale(v, scale));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| set_surface_scale(v, scale)),
        _ => {}
    }
}

/// The liquid surface on the frame's seam outputs: frame B's particles and
/// count, its solid lattice, and the lattice's bounds and nodes.
fn surface(b: &mut Builder, scene: WaterScene, frame: usize) -> Port {
    let mut group = surface_group();
    set_surface_scale(&mut group, scene.surface_scale);
    let id = b.nodes.iter().map(|node| node.id as usize + 1).max().unwrap_or(0);
    group["id"] = json!(id);
    group["nodeId"] = json!("surface");
    b.nodes.push(serde_json::from_value(group).expect("surface group"));
    for (from, to) in [
        ("particles_b", "particles"),
        ("interior_b", "interior"),
        ("count_b", "count"),
        ("solid_b", "solid"),
        ("grid_bounds", "bounds"),
        ("grid_nodes_x", "nodes_x"),
        ("grid_nodes_y", "nodes_y"),
        ("grid_nodes_z", "nodes_z"),
    ] {
        b.wire((frame, from), id, to);
    }
    (id, "vertices")
}

/// The scene's padded lattice (the domain's `LATTICE_WIRES`) as params,
/// plus `extra`. The wires carry any change at run time.
fn padded_lattice(scene: &WaterScene, extra: &[(&str, Value)]) -> Value {
    let lattice = scene.geometry().setup.lattice;
    let mut params = json!({"cell_size": float(f64::from(lattice.cell_size()))});
    for (axis, name) in ["lattice_min_x", "lattice_min_y", "lattice_min_z"].into_iter().enumerate() {
        params[name] = float(f64::from(lattice.min()[axis]));
    }
    for (axis, name) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().enumerate() {
        params[name] = int(lattice.nodes()[axis] as usize);
    }
    for (name, value) in extra {
        params[*name] = value.clone();
    }
    params
}

/// The tick's water (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)):
/// one node.gpu_flip_step of `scene.steps` substeps on the domain's lattice, gravity,
/// fields and bodies, and the state's tick index. The caller wires its
/// particles and count.
fn water_step(b: &mut Builder, scene: WaterScene, tick: (usize, usize)) -> usize {
    let (domain, state) = tick;
    let iterations = |n: usize, auto: u32| int(if n == auto as usize { 0 } else { n });
    let step = b.node(
        STEP_NODE,
        "node.gpu_flip_step",
        padded_lattice(
            &scene,
            &[
                ("steps", int(scene.steps)),
                ("flip", float(scene.flip)),
                ("iterations", iterations(scene.pressure.iterations, PRESSURE_ITERATIONS as u32)),
                ("ghost_fluid", int(usize::from(scene.ghost_fluid))),
                ("volume_projection", int(usize::from(scene.volume_projection))),
            ],
        ),
    );
    b.wires(domain, step, &LATTICE_WIRES);
    b.wire((domain, "interval_duration"), step, "interval_duration");
    b.wire((domain, "gravity_x"), step, "gravity_x");
    b.wire((domain, "gravity"), step, "gravity_y");
    b.wire((domain, "gravity_z"), step, "gravity_z");
    b.wires(domain, step, &FIELD_WIRES);
    b.wire((state, "tick_index"), step, "tick_index");
    b.wire((state, "retired_max_speed"), step, "retired_max_speed");
    b.wire((domain, "initial_obstacle_speed"), step, "initial_obstacle_speed");
    b.wires(domain, step, &["bodies", "contacts", "shapes", "atlas", "body_count", "dynamic_bodies", "closed_faces", "solve_level", "max_iterations", "sheet_fill_rate"]);
    b.wire((domain, "body_rows"), step, "rows");
    step
}

/// Device bytes a scene holds inside the render graph at 1920×1080, as the
/// liquid extent check counts them: every array at the size the walk reached
/// plus what each node holds for itself. Textures are not counted.
#[cfg(any(any(test, feature = "testkit"), feature = "testkit"))]
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    crate::testkit::liquid_extents::walk(&render_def(scene), false).expect("the rendered scene covers every dispatch").scene_bytes
}

/// `def` as the app renders a generator: fused when it has regions, `None`
/// when it has none and runs as authored. Regions that refuse to fuse fail.
#[cfg(any(any(test, feature = "testkit"), feature = "testkit"))]
pub(super) fn fused_as_rendered(
    def: &EffectGraphDef,
    registry: &manifold_node_engine::persistence::PrimitiveRegistry,
) -> Option<manifold_node_engine::freeze::install::FusedGeneratorView> {
    let view = manifold_node_engine::freeze::install::fuse_generator_view(def, registry);
    if view.is_none() {
        let regions = manifold_node_engine::freeze::fusion_report(def, registry).regions;
        assert!(regions.is_empty(), "the def has {} fusable regions but does not fuse", regions.len());
    }
    view
}

/// Independent axis oracle over the same solver output and tick region.
#[cfg(any(test, feature = "testkit"))]
pub fn with_whitewater_axes(def: EffectGraphDef) -> EffectGraphDef {
    // This independent axis oracle edits the compiled topology, including nested
    // surface nodes. Production presets retain their authored groups.
    let def = manifold_core::flatten::flatten_groups(&def).expect("axis oracle flattens");
    let mut def = serde_json::to_value(def).expect("def serialises");
    let nodes = def["nodes"].as_array().expect("nodes");
    let id = |name: &str| nodes.iter().find(|n| n["nodeId"] == name).expect("node")["id"].as_u64().expect("id");
    let (step, domain, whitewater) = (id("step"), id("domain"), id("whitewater"));
    let mut next = nodes.iter().filter_map(|n| n["id"].as_u64()).max().unwrap() + 1;
    let gone: Vec<_> = nodes.iter().filter(|n| ["whitewater_face_u", "whitewater_face_v", "whitewater_face_w"].iter().any(|name| n["nodeId"] == *name)).map(|n| n["id"].clone()).collect();
    def["nodes"].as_array_mut().unwrap().retain(|n| !gone.contains(&n["id"]));
    def["wires"].as_array_mut().unwrap().retain(|w| !(gone.contains(&w["fromNode"]) || gone.contains(&w["toNode"])
        || w["toNode"] == whitewater && ["faces", "face_u", "face_v", "face_w"].iter().any(|port| w["toPort"] == *port)));
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        let adapter = next;
        next += 1;
        def["nodes"].as_array_mut().unwrap().push(serde_json::json!({"id": adapter, "nodeId": format!("whitewater_{port}"), "typeId": "node.face_sample_component", "params": {"axis": {"type": "Enum", "value": axis}}}));
        let wires = def["wires"].as_array_mut().unwrap();
        wires.push(serde_json::json!({"fromNode": step, "fromPort": "faces", "toNode": adapter, "toPort": "faces"}));
        for dim in ["x", "y", "z"] {
            wires.push(serde_json::json!({"fromNode": domain, "fromPort": format!("nodes_{dim}"), "toNode": adapter, "toPort": format!("nodes_{dim}")}));
        }
        wires.push(serde_json::json!({"fromNode": adapter, "fromPort": "out", "toNode": whitewater, "toPort": port}));
    }
    serde_json::from_value(def).expect("def with axis adapters")
}

// CPU size proofs for every GPU FLIP graph, run before any GPU run of it: the
// shared liquid extent rules (`liquid::extent`) at every lattice, bare,
// meshed, rendered and frozen.

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testkit;
