//! GPU FLIP, the GPU water solver (docs/GPU_FLIP_PRESSURE_SOLVE.md), as
//! graphs built for any lattice. `water_def` is a running liquid on the
//! liquid seam (docs/LIQUID_SOLVER_SEAM_DESIGN.md P7a): node.gpu_flip_domain's clock runs
//! node.liquid_state's tick region, whose body is one 60 Hz tick of
//! [`STEPS_PER_TICK`] water steps, the density solve on the last, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakGpuFlip.json`, which is its own Dam Break at 64. The solver's
//! lattice is baked into its atoms' params, and the domain refuses any other
//! (lifted in P7b).

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use serde_json::{Value, json};

use super::coarse_inverse::multigrid_levels;
use super::gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry};
use crate::node_graph::bundled_presets::bundled_preset_json;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::fluid::{FluidDomainLayout, domain_layout};
use crate::node_graph::liquid::grid::FACE_INPUT_PORTS;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::transform::Transform;

/// The box is 4 m on its longest side; the lowest wave it holds is 2π / 4 m.
pub(crate) const BOX_METRES: f64 = 4.0;

/// Water steps per 60 Hz liquid tick (D8): copies of the step inside the tick
/// region, the density solve on the last. A builder constant: whether a
/// coupled body moves per step or per tick is the solids work's to settle (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (owed)).
pub(crate) const STEPS_PER_TICK: usize = 2;

/// Iteration counts the GPU iteration trend runs; the CPU size proof covers each.
#[cfg(test)]
pub(super) const TREND_ITERATIONS: [usize; 5] = [3, 4, 6, 8, 12];

/// The main solve's iterations at every lattice (Auto). A multigrid
/// preconditioner's count does not grow with the lattice: on the seven Dam
/// Break problems and the dumped splash solves, the f64 reference needed at
/// most 7 iterations at 64³ and 5 at 128³ to reach the retired FFT solve's residual
/// (`scripts/mgpcg_reference.py`, docs/GPU_FLIP_PRESSURE_SOLVE.md). One more
/// is the margin.
pub(crate) const PRESSURE_ITERATIONS: usize = 8;

/// The density solve's iterations (Auto): the reference matched the retired
/// FFT density solve's residual in 2 at 64³ and 1 at 128³, plus one.
pub(crate) const DENSITY_ITERATIONS: usize = 3;

/// Red-black sweeps before and after each coarse correction.
const SMOOTH_SWEEPS: usize = 2;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PressureShape {
    /// Cells per side of the cubic lattice.
    pub n: usize,
    /// Conjugate gradient iterations, one V-cycle each.
    pub iterations: usize,
}

impl PressureShape {
    pub fn at(n: usize) -> Self {
        Self { n, iterations: PRESSURE_ITERATIONS }
    }

    pub fn cells(&self) -> usize {
        self.n * self.n * self.n
    }

    pub fn cell_size(&self) -> f64 {
        BOX_METRES / self.n as f64
    }

    /// The V-cycle's lattice sides, finest first, as the domain's refusal
    /// counts them.
    pub fn levels(&self) -> Vec<usize> {
        multigrid_levels([self.n as u32; 3]).iter().map(|level| level[0] as usize).collect()
    }
}

/// The FLIP Fluids engine's Dam Break (`WaterDamBreak.json`) with its
/// obstacle unwired: a 4 m tank over the floor, a 0.16 m pool, and the
/// `initial_column` block, seeded by the engine's half-cell site rule.
pub(crate) const DAM_FILL_HEIGHT: f64 = 0.16;
pub(crate) const DAM_COLUMN: [[f64; 2]; 3] = [[-1.84, -0.66], [0.16, 2.08], [-1.75, 1.75]];

/// A liquid in the 4 m tank: a pool `fill_height` deep plus one box, both
/// in metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaterScene {
    pub pressure: PressureShape,
    /// Water steps per tick: copies of the step subgraph in the tick region.
    pub steps: usize,
    /// The FLIP share kept per 1/60 s, the FLIP Fluids engine's 0.95 at its
    /// one step a frame; [`Self::flip_per_step`] is what a step uses.
    pub flip: f64,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// How fast crowded cells spread (1/s): node.density_source's rate. 0
    /// leaves the density solve out.
    pub spread_rate: f64,
    /// Iterations of the density solve. It moves particles and is never kept
    /// as velocity, so its leftover error shows as a slightly uneven spread,
    /// not as motion.
    pub density_iterations: usize,
    /// Run the density solve on the tick's last step only. The spread rate
    /// stays per step, so that one solve removes the same share. On in the
    /// shipped cadence: at 64³ it holds the water measures within 1.5 points
    /// of a solve every step (`gpu_flip_cadence_64`) and saves one solve.
    pub density_once: bool,
    /// Surface lattice nodes per cell (`resolution_scale` of the surface's
    /// volume and mesh): the shipped Surface Detail 0 is 2, which fits the
    /// frame budget at 64 (BUG-mjhx, surface scale at GPU FLIP 64).
    pub surface_scale: usize,
    /// Publish the face grid: three node.face_sample_component named
    /// [`FACE_NODES`] on the state's faces after the region, into the frame.
    /// The tick always hands its last step's faces to the state.
    pub faces: bool,
}

/// The face grid's nodes in a scene built with `faces`, x, y and z.
pub(crate) const FACE_NODES: [&str; 3] = ["face_u", "face_v", "face_w"];

/// Face layers past the water that `old` and `advect` are extended by. They
/// are sampled only where a particle starts its step, inside a water cell,
/// and a sample reads faces one cell out.
pub(crate) const EXTENDED_LAYERS: usize = 2;

/// The fastest water a step is built for (m/s): the Dam Break's splash tops
/// out near 13 m/s at 64³ and 19–25 m/s at 128³, the FLIP Fluids engine's
/// at 12 and 17. [`WaterScene::travel_cells`] turns it into the CFL guard.
pub(crate) const TOP_SPEED: f64 = 20.0;

/// Particles per cell the fill seeds: one per half-cell site.
pub(crate) const REST_PER_CELL: f64 = 8.0;

/// The share of a cell's crowding one density solve removes: spread_rate ×
/// step dt. Crowding goes as (1 − share) per solve, so 1 removes it in one.
/// It holds at 64³ and 128³ because node.density_source counts only half-full
/// neighbours as water and node.faces_to_particles caps the move at half a
/// cell (`gpu_flip_refined_density_causes`).
pub(crate) const SPREAD_PER_STEP: f64 = 1.0;

impl WaterScene {
    /// The engine's Dam Break, obstacle unwired.
    pub fn dam_break(n: usize) -> Self {
        Self {
            pressure: PressureShape::at(n),
            steps: STEPS_PER_TICK,
            flip: 0.95,
            fill_height: DAM_FILL_HEIGHT,
            column: DAM_COLUMN,
            surface: false,
            spread_rate: SPREAD_PER_STEP * 60.0 * STEPS_PER_TICK as f64,
            density_iterations: DENSITY_ITERATIONS,
            density_once: true,
            surface_scale: 2,
            faces: false,
        }
    }

    /// A pool 1 m deep and nothing else (I5).
    pub fn still_pool(n: usize) -> Self {
        Self { fill_height: 1.0, column: [[0.0; 2]; 3], ..Self::dam_break(n) }
    }

    /// A pool 3 m deep in the 4 m tank: the coarsest multigrid level is
    /// water but for its top row, the hardest case for the coarse solve.
    #[cfg(test)]
    pub fn deep_pool(n: usize) -> Self {
        Self { fill_height: 3.0, ..Self::still_pool(n) }
    }

    /// The deep pool with a 1 m × 0.5 m × 1 m block dropped in from 0.2 m
    /// above: a still pool's density source is zero, this one crowds.
    #[cfg(test)]
    pub fn deep_drop(n: usize) -> Self {
        Self { column: [[-0.5, 0.5], [3.2, 3.7], [-0.5, 0.5]], ..Self::deep_pool(n) }
    }

    /// A 1 m block of water high in the tank, clear of every wall.
    #[cfg(test)]
    pub fn free_fall(n: usize) -> Self {
        Self { fill_height: 0.0, column: [[-0.5, 0.5], [2.5, 3.5], [-0.5, 0.5]], ..Self::dam_break(n) }
    }

    /// Density solves a tick: none, the last step's, or every step's.
    #[cfg(test)]
    pub fn density_solves(&self) -> usize {
        match (self.spread_rate > 0.0, self.density_once) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => self.steps,
        }
    }

    pub fn with_surface(self) -> Self {
        Self { surface: true, ..self }
    }

    /// Publish the face grid (section 3.2 (Grid outputs) of the seam).
    pub fn with_faces(self) -> Self {
        Self { faces: true, ..self }
    }

    /// Meshed at `scale` surface nodes per cell.
    #[cfg(test)]
    pub fn with_surface_scale(self, scale: usize) -> Self {
        Self { surface: true, surface_scale: scale, ..self }
    }

    /// The same scene with `iterations` per pressure solve.
    #[cfg(test)]
    pub fn with_iterations(self, iterations: usize) -> Self {
        Self { pressure: PressureShape { iterations, ..self.pressure }, ..self }
    }

    /// `steps` water steps a frame, each density solve's share kept.
    #[cfg(test)]
    pub fn with_steps(self, steps: usize) -> Self {
        Self { steps, spread_rate: SPREAD_PER_STEP * 60.0 * steps as f64, ..self }
    }

    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    /// node.faces_to_particles' FLIP share for one step. `flip` is the share
    /// kept per 1/60 s, so the PIC damping a second does not depend on the
    /// step count: k steps a frame keep flip^(1/k) each.
    pub fn flip_per_step(&self) -> f64 {
        self.flip.powf(60.0 * self.step_dt())
    }

    /// The CFL guard: the farthest one RK3 stage moves a particle, in cells,
    /// [`TOP_SPEED`] for one step rounded up. Faster water keeps its speed
    /// and moves this far.
    pub fn travel_cells(&self) -> usize {
        (TOP_SPEED * self.step_dt() / self.pressure.cell_size() - 1e-9).ceil().max(1.0) as usize
    }

    /// Layers `new` is extended by: the RK3 stages sample up to ¾ of the
    /// travel from where the particle started, and a sample reads faces one
    /// cell further. The face grid's `face_valid_layers`.
    pub fn band_layers(&self) -> usize {
        (0.75 * self.travel_cells() as f64).ceil() as usize + 1
    }

    /// The tank: the domain's layout at this resolution, no domain box.
    pub fn layout(&self) -> FluidDomainLayout {
        domain_layout(None, BOX_METRES as f32, self.pressure.n as u32).expect("the tank's layout")
    }

    /// The tank's lowest corner.
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
        let mut params = ParamValues::default();
        params.insert("built_resolution".into(), ParamValue::Float(n));
        params.insert("built_domain_size".into(), ParamValue::Float(BOX_METRES as f32));
        let read = |name: &str, default: f32| match name {
            "resolution" => n,
            "domain_size" => BOX_METRES as f32,
            "fill_height" => self.fill_height as f32,
            _ => default,
        };
        gpu_flip_geometry(read, &params, None, self.initial_volume()).expect("the scene fits its domain")
    }

    #[cfg(test)]
    pub fn pool_sites(&self) -> u32 {
        self.geometry().setup.pool_sites
    }

    #[cfg(test)]
    pub fn box_sites(&self) -> [[u32; 2]; 3] {
        self.geometry().setup.box_sites
    }

    /// Particles the fill places; the particle arrays hold exactly this many.
    #[cfg(test)]
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

/// An array capacity as an Int param. Params are f32, which counts exactly
/// only to 2²⁴ (257³ is past it); above that the nearest f32 at or above `v`
/// is used, so the array is never short.
#[cfg(test)]
fn capacity(v: usize) -> Value {
    let mut f = v as f32;
    if (f as u64) < v as u64 {
        f = f32::from_bits(f.to_bits() + 1);
    }
    json!({"type": "Int", "value": f as u64})
}

#[derive(Default)]
struct Builder {
    nodes: Vec<Value>,
    wires: Vec<Value>,
    /// Prepended to every node name: each water step's copy of the solve
    /// needs its own.
    prefix: String,
}

type Port = (usize, &'static str);

impl Builder {
    fn node(&mut self, name: &str, type_id: &str, params: Value) -> usize {
        let id = self.nodes.len();
        let name = format!("{}{name}", self.prefix);
        self.nodes.push(json!({"id": id, "nodeId": name, "typeId": type_id, "params": params}));
        id
    }

    fn wire(&mut self, from: Port, to: usize, port: &str) {
        self.wires.push(json!({"fromNode": from.0, "fromPort": from.1, "toNode": to, "toPort": port}));
    }

    /// The same-named scalar outputs of `from` into `to`.
    fn wires(&mut self, from: usize, to: usize, ports: &[&'static str]) {
        for &port in ports {
            self.wire((from, port), to, port);
        }
    }

    /// Lattice params of the transform atoms.
    fn lattice(nodes: [usize; 3], extra: &[(&str, Value)]) -> Value {
        let mut params = json!({
            "nodes_x": float(nodes[0] as f64),
            "nodes_y": float(nodes[1] as f64),
            "nodes_z": float(nodes[2] as f64),
        });
        for (name, value) in extra {
            params[*name] = value.clone();
        }
        params
    }

    /// a · b over the first `length` elements, into one value.
    fn dot(&mut self, name: &str, a: Port, b: Port, length: usize) -> Port {
        let id = self.node(name, "node.dot_products", json!({"row_length": int(length), "rows": int(1), "max_rows": int(1)}));
        self.wire(a, id, "matrix");
        self.wire(b, id, "vector");
        (id, "out")
    }

    /// values / divisor[0], zeros when the divisor is under 1e-30.
    fn divide(&mut self, name: &str, values: Port, divisor: Port) -> Port {
        let id = self.node(name, "node.divide_by_value", json!({}));
        self.wire(values, id, "values");
        self.wire(divisor, id, "divisor");
        (id, "out")
    }

    /// base + scale · coef[0] · vector.
    fn axpy(&mut self, name: &str, base: Port, vector: Port, coef: Port, scale: f64, length: usize) -> Port {
        let id = self.node(
            name,
            "node.combine_rows",
            json!({"row_length": int(length), "rows": int(1), "scale": float(scale), "base_scale": float(1.0)}),
        );
        self.wire(base, id, "base");
        self.wire(vector, id, "matrix");
        self.wire(coef, id, "coef");
        (id, "out")
    }
}

/// The whole solve for one step, from the water lattice and its divergence:
/// the coarse levels, then the conjugate gradient region.
#[cfg(test)]
pub(super) fn pressure_def(s: PressureShape) -> EffectGraphDef {
    let mut b = Builder::default();
    let cells = s.cells();
    let water = b.node("water", "test.value_source", json!({"max_capacity": capacity(cells)}));
    let f = b.node("f", "test.value_source", json!({"max_capacity": capacity(cells)}));
    let faces = b.node("solid_faces", "test.face_source", json!({"max_capacity": capacity((s.n + 1).pow(3))}));
    let levels = levels(&mut b, s, (water, "out"), (faces, "out"));
    let pressure = solve(&mut b, s, &levels, (f, "out"));
    let sink = b.node("sink", "test.value_sink", json!({}));
    b.wire(pressure, sink, "values");
    let output = b.node("output", "system.final_output", json!({}));
    b.wire((sink, "out"), output, "in");
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("pressure def")
}

/// The padded lattice's scalars, as the domain publishes them.
const LATTICE_WIRES: [&str; 7] = ["lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z"];

/// The fill's sites, as the domain publishes them.
const FILL_WIRES: [&str; 7] = ["pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1"];

/// A scene as a running liquid on the seam. The domain seeds the fill and
/// runs the clock; the state's region runs one tick per due tick: `steps`
/// water steps from `state.out`, then the tick's stats, closing into
/// `state.in` and `state.stats_in`. The frame publishes each tick with the
/// solid lattice. The harness sink holds the frame, or the surface mesh when
/// `surface`.
pub(crate) fn water_def(scene: WaterScene) -> EffectGraphDef {
    let mut b = Builder::default();
    let s = scene.pressure;
    let geometry = scene.geometry();
    let domain = b.node(
        "domain",
        GPU_FLIP_DOMAIN_TYPE_ID,
        json!({
            "resolution": int(s.n),
            "domain_size": float(BOX_METRES),
            "fill_height": float(scene.fill_height),
            "built_resolution": int(s.n),
            "built_domain_size": float(BOX_METRES),
        }),
    );
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
    // The params hold the domain's own sites, so the planned storage is the
    // fill's; the wires carry any change.
    let sites = geometry.setup.box_sites;
    let fill = b.node(
        "fill",
        "node.liquid_fill",
        lattice_box(
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
    let count = (fill, "count");
    let state = b.node("state", "node.liquid_state", json!({}));
    b.wire((fill, "particles"), state, "seed");
    b.wire(count, state, "count");
    b.wires(domain, state, &["ticks", "epoch"]);
    let mut particles: Port = (state, "out");
    let mut faces = particles;
    for k in 0..scene.steps {
        b.prefix = format!("s{k}.");
        let density = scene.spread_rate > 0.0 && (!scene.density_once || k + 1 == scene.steps);
        (particles, faces) = water_step(&mut b, scene, particles, count, domain, density, k);
    }
    b.prefix.clear();
    let stats = b.node("stats", "node.liquid_stats", json!({}));
    b.wire(particles, stats, "particles");
    b.wire((state, "stats"), stats, "stats");
    b.wire(count, stats, "count");
    b.wire((domain, "particle_mass"), stats, "particle_mass");
    b.wire(particles, state, "in");
    b.wire((stats, "stats_out"), state, "stats_in");
    // The tick's last faces leave the region beside its particles.
    b.wire(faces, state, "faces_in");

    let solid = b.node("solid", "node.liquid_solid_distance", json!({}));
    b.wires(domain, solid, &["bodies", "shapes", "atlas", "closed_faces", "body_count"]);
    b.wires(domain, solid, &LATTICE_WIRES);
    b.wire((domain, "body_rows"), solid, "rows");
    let frame = b.node("frame", "node.liquid_frame", json!({"face_valid_layers": int(scene.band_layers())}));
    b.wire((state, "out"), frame, "particles");
    b.wire((state, "stats"), frame, "stats");
    b.wire((solid, "solid"), frame, "solid");
    b.wire(count, frame, "count");
    b.wires(domain, frame, &LATTICE_WIRES);
    b.wires(domain, frame, &["closed_faces", "simulation_time", "display_time", "epoch"]);
    if scene.faces {
        for (axis, name) in FACE_NODES.into_iter().enumerate() {
            let params = Builder::lattice([scene.pressure.n; 3], &[("axis", json!({"type": "Enum", "value": axis}))]);
            let id = b.node(name, "node.face_sample_component", params);
            b.wire((state, "faces"), id, "faces");
            b.wire((id, "out"), frame, FACE_INPUT_PORTS[axis]);
        }
    }

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

/// The shipped GPU FLIP Dam Break. Its render (camera, lights, environment,
/// tank, water material, tone map) and its Liquid Surface group are the one
/// source every builder scene takes them from; its water is the builder's
/// Dam Break at 64 (`gpu_flip_shipped_preset_is_the_builders_dam_break`).
pub(crate) const SHIPPED_PRESET: &str = "WaterDamBreakGpuFlip";

fn shipped_preset() -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(SHIPPED_PRESET)).expect("the GPU FLIP preset is bundled");
    serde_json::from_str(&json).expect("the GPU FLIP preset parses")
}

/// The shipped preset's Liquid Surface group.
fn surface_group() -> Value {
    let preset = shipped_preset();
    let nodes = preset["nodes"].as_array().expect("preset nodes");
    nodes.iter().find(|node| node["nodeId"] == "surface").expect("liquid surface group").clone()
}

/// The nodes `water_def` makes; every other node of the shipped preset is
/// its render.
fn built_by_water_def(node_id: &str) -> bool {
    let step = node_id.split_once('.').is_some_and(|(step, _)| {
        step.len() > 1 && step.starts_with('s') && step[1..].bytes().all(|b| b.is_ascii_digit())
    });
    step || FACE_NODES.contains(&node_id)
        || matches!(node_id, "domain" | "initial_column" | "fill" | "state" | "stats" | "solid" | "frame" | "surface")
}

/// Surface Detail adds this to its value to give the surface nodes' scale.
const SURFACE_DETAIL_OFFSET: usize = 2;

/// A meshed scene inside the shipped preset's render: `water_def`'s water
/// and surface, the preset's other nodes, and the preset's wires between the
/// two, matched by node name.
pub(crate) fn render_def(scene: WaterScene) -> EffectGraphDef {
    // The render's Whitewater group reads the face grid.
    let mut def = serde_json::to_value(water_def(scene.with_surface().with_faces())).expect("water def serialises");
    let preset = shipped_preset();
    let id_of = |graph: &Value, name: &str| -> u64 {
        let nodes = graph["nodes"].as_array().expect("nodes");
        let node = nodes.iter().find(|n| n["nodeId"] == name).unwrap_or_else(|| panic!("no node {name}"));
        node["id"].as_u64().expect("numeric id")
    };
    let harness = [id_of(&def, "mesh_sink"), id_of(&def, "output")];
    let ends = |wire: &Value| [wire["fromNode"].as_u64().expect("from"), wire["toNode"].as_u64().expect("to")];
    def["nodes"].as_array_mut().expect("nodes").retain(|n| !harness.contains(&n["id"].as_u64().expect("id")));
    def["wires"].as_array_mut().expect("wires").retain(|w| ends(w).iter().all(|id| !harness.contains(id)));
    let next = def["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1;
    let name_of = |node: &Value| node["nodeId"].as_str().expect("node name").to_string();
    let preset_nodes = preset["nodes"].as_array().expect("preset nodes");
    let render: Vec<&Value> = preset_nodes.iter().filter(|n| !built_by_water_def(&name_of(n))).collect();
    let first = render.iter().filter_map(|n| n["id"].as_u64()).min().expect("the preset has a render");
    // Render ids move up only when this scene's water outgrows the preset's.
    let shift = next.saturating_sub(first);
    let render_ids: Vec<u64> = render.iter().map(|n| n["id"].as_u64().expect("preset id")).collect();
    for node in &render {
        let mut node = (*node).clone();
        node["id"] = json!(node["id"].as_u64().expect("preset id") + shift);
        def["nodes"].as_array_mut().expect("nodes").push(node);
    }
    let water_name: Vec<(u64, String)> = preset_nodes
        .iter()
        .filter(|n| built_by_water_def(&name_of(n)))
        .map(|n| (n["id"].as_u64().expect("preset id"), name_of(n)))
        .collect();
    for wire in preset["wires"].as_array().expect("preset wires") {
        if ends(wire).iter().all(|id| !render_ids.contains(id)) {
            continue;
        }
        let mut wire = wire.clone();
        for end in ["fromNode", "toNode"] {
            let id = wire[end].as_u64().expect("end");
            wire[end] = json!(if render_ids.contains(&id) {
                id + shift
            } else {
                let name = &water_name.iter().find(|(water, _)| *water == id).expect("a preset node").1;
                id_of(&def, name)
            });
        }
        def["wires"].as_array_mut().expect("wires").push(wire);
    }
    for key in ["name", "description"] {
        def[key] = preset[key].clone();
    }
    def["presetMetadata"] = scene_cards(&preset["presetMetadata"], scene);
    serde_json::from_value(def).expect("render def")
}

/// The shipped cards with every default at the value this scene's def bakes,
/// so no card overwrites what the extent proof checked: Resolution at the
/// lattice, Surface Detail at the surface scale, gone past its range.
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

/// Sets `resolution_scale` on every surface volume and mesh node in `value`,
/// however deep the group nests them.
fn set_surface_scale(value: &mut Value, scale: usize) {
    match value {
        Value::Object(map) => {
            let surface = map.get("nodeId").is_some_and(|id| id == "liquid_volume" || id == "liquid_mesh");
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
    let id = b.nodes.len();
    group["id"] = json!(id);
    group["nodeId"] = json!("surface");
    b.nodes.push(group);
    for (from, to) in [
        ("particles_b", "particles"),
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

/// Lattice params plus the lattice's box: cell size and lowest corner.
fn lattice_box(scene: &WaterScene, extra: &[(&str, Value)]) -> Value {
    let s = scene.pressure;
    let mut params = Builder::lattice([s.n; 3], extra);
    params["cell_size"] = float(s.cell_size());
    let min = scene.min();
    for (axis, name) in ["lattice_min_x", "lattice_min_y", "lattice_min_z"].into_iter().enumerate() {
        params[name] = float(min[axis]);
    }
    params
}

/// One water step (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)):
/// sort, the water lattice, particles to faces, the domain's gravity, the
/// pressure solve, the projection, the density solve when `density`, faces
/// back to particles.
/// Returns the moved particles and the step's projected, extended faces.
fn water_step(
    b: &mut Builder,
    scene: WaterScene,
    particles: Port,
    count: Port,
    domain: usize,
    density: bool,
    step: usize,
) -> (Port, Port) {
    let s = scene.pressure;
    let n = [s.n; 3];
    let h = s.cell_size();
    let dt = scene.step_dt();
    let side = BOX_METRES;
    let min = scene.min();
    let sort = b.node(
        "sort",
        "node.sort_particles_into_cells",
        json!({
            "center_x": float(min[0] + 0.5 * side),
            "center_y": float(min[1] + 0.5 * side),
            "center_z": float(min[2] + 0.5 * side),
            "size_x": float(side),
            "size_y": float(side),
            "size_z": float(side),
            "cell_size": float(h),
        }),
    );
    b.wire(particles, sort, "particles");
    b.wire(count, sort, "count");
    let water = b.node("water", "node.cells_with_particles", Builder::lattice(n, &[]));
    b.wire((sort, "cell_ranges"), water, "cell_ranges");
    let water = (water, "out");
    let gather = b.node("faces", "node.particles_to_faces", lattice_box(&scene, &[]));
    b.wire((sort, "sorted"), gather, "sorted");
    b.wire((sort, "cell_ranges"), gather, "cell_ranges");
    let old = extend(b, "old", (gather, "out"), n, EXTENDED_LAYERS);
    let forced = b.node("gravity", "node.face_gravity", Builder::lattice(n, &[("step_dt", float(dt))]));
    b.wire(old, forced, "faces");
    b.wire((domain, "gravity_x"), forced, "gravity_x");
    b.wire((domain, "gravity"), forced, "gravity_y");
    b.wire((domain, "gravity_z"), forced, "gravity_z");
    let (solid, solid_velocity) = solid_faces(b, scene, domain, (step + 1) as f64 * dt);
    let divergence = b.node("divergence", "node.face_divergence", Builder::lattice(n, &[("cell_size", float(h))]));
    b.wire((forced, "out"), divergence, "faces");
    b.wire(water, divergence, "water");
    b.wire(solid, divergence, "solid_faces");
    b.wire(solid_velocity, divergence, "solid_velocity");
    let levels = levels(b, s, water, solid);
    let p = solve(b, s, &levels, (divergence, "out"));
    let projected = subtract(b, "project", (forced, "out"), p, &levels, s);
    // The engine constrains its velocity and its saved velocity to the
    // solids after the pressure solve, so FLIP's change is measured between
    // two constrained fields.
    let projected = constrain(b, "constrain", projected, solid, solid_velocity, n);
    let old = constrain(b, "old_constrain", old, solid, solid_velocity, n);
    let new = extend(b, "new", projected, n, scene.band_layers());
    // The density solve moves particles apart through `advect` and is never
    // kept as velocity: kept, a fast splash's correction becomes speed.
    let advect = if density {
        let outer = b.prefix.clone();
        b.prefix.push_str("density.");
        let crowding = b.node(
            "source",
            "node.density_source",
            Builder::lattice(n, &[("rest", float(REST_PER_CELL)), ("rate", float(scene.spread_rate))]),
        );
        b.wire((sort, "cell_ranges"), crowding, "cell_ranges");
        let q = solve(b, PressureShape { iterations: scene.density_iterations, ..s }, &levels, (crowding, "out"));
        let spread = subtract(b, "project", projected, q, &levels, s);
        let advect = extend(b, "advect", spread, n, EXTENDED_LAYERS);
        b.prefix = outer;
        advect
    } else {
        new
    };
    let moved = b.node(
        "move",
        "node.faces_to_particles",
        lattice_box(
            &scene,
            &[("step_dt", float(dt)), ("flip", float(scene.flip_per_step())), ("max_travel", float(scene.travel_cells() as f64))],
        ),
    );
    b.wire((sort, "sorted"), moved, "particles");
    b.wire(new, moved, "faces");
    b.wire(old, moved, "old");
    b.wire(advect, moved, "advect");
    ((moved, "out"), new)
}

/// `faces` minus the gradient of `pressure` on the water's faces.
fn subtract(b: &mut Builder, name: &str, faces: Port, pressure: Port, levels: &Levels, s: PressureShape) -> Port {
    let n = [s.n; 3];
    let id = b.node(name, "node.subtract_pressure", Builder::lattice(n, &[("cell_size", float(s.cell_size()))]));
    b.wire(faces, id, "faces");
    b.wire(pressure, id, "pressure");
    b.wire(levels.water[0], id, "water");
    b.wire(levels.faces[0], id, "solid_faces");
    (id, "out")
}

/// The step's face open fractions: the domain's bodies as a solid distance on
/// the box's corner lattice, posed `seconds` into the tick, then each face's
/// open fraction. The box walls are the faces' own, so the lattice has none.
/// `faces` with the solids' velocity on the faces they close or cut.
fn constrain(b: &mut Builder, name: &str, faces: Port, solid: Port, solid_velocity: Port, n: [usize; 3]) -> Port {
    let id = b.node(name, "node.constrain_solid_faces", Builder::lattice(n, &[]));
    b.wire(faces, id, "faces");
    b.wire(solid, id, "solid_faces");
    b.wire(solid_velocity, id, "solid_velocity");
    (id, "out")
}

/// The solids' open fraction per face, and their velocity and friction there.
fn solid_faces(b: &mut Builder, scene: WaterScene, domain: usize, seconds: f64) -> (Port, Port) {
    let s = scene.pressure;
    let min = scene.min();
    let corners = s.n + 1;
    let distance = b.node(
        "solid",
        "node.liquid_solid_distance",
        json!({
            "lattice_min_x": float(min[0]),
            "lattice_min_y": float(min[1]),
            "lattice_min_z": float(min[2]),
            "cell_size": float(s.cell_size()),
            "nodes_x": int(corners),
            "nodes_y": int(corners),
            "nodes_z": int(corners),
            "closed_faces": int(0),
            "tick_seconds": float(seconds),
        }),
    );
    b.wires(domain, distance, &["bodies", "shapes", "atlas", "body_count"]);
    b.wire((domain, "body_rows"), distance, "rows");
    let reach = min.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let open = b.node(
        "solid_faces",
        "node.solid_faces",
        Builder::lattice([s.n; 3], &[("cell_size", float(s.cell_size())), ("box_offset", float(reach))]),
    );
    b.wire((distance, "solid"), open, "solid");
    let velocity = b.node(
        "solid_velocity",
        "node.solid_face_velocity",
        json!({
            "lattice_min_x": float(min[0]),
            "lattice_min_y": float(min[1]),
            "lattice_min_z": float(min[2]),
            "cell_size": float(s.cell_size()),
            "nodes_x": int(s.n),
            "nodes_y": int(s.n),
            "nodes_z": int(s.n),
            "tick_seconds": float(seconds),
        }),
    );
    b.wire((open, "out"), velocity, "solid_faces");
    b.wires(domain, velocity, &["bodies", "shapes", "atlas", "body_count"]);
    b.wire((domain, "body_rows"), velocity, "rows");
    ((open, "out"), (velocity, "out"))
}

/// `layers` layers of face extension into the air around the water.
fn extend(b: &mut Builder, name: &str, faces: Port, n: [usize; 3], layers: usize) -> Port {
    let mut faces = faces;
    for layer in 1..=layers {
        let id = b.node(&format!("{name}_extend_{layer}"), "node.extend_faces", Builder::lattice(n, &[]));
        b.wire(faces, id, "faces");
        faces = (id, "out");
    }
    faces
}

/// What every solve on one water lattice shares: the water at each V-cycle
/// level, finest first, a zero lattice per level for the sweeps to start
/// from, and the coarsest level's inverse.
struct Levels {
    water: Vec<Port>,
    faces: Vec<Port>,
    zeros: Vec<Port>,
    inverse: Port,
}

fn levels(b: &mut Builder, s: PressureShape, water: Port, faces: Port) -> Levels {
    let sides = s.levels();
    let mut water_levels = vec![water];
    let mut face_levels = vec![faces];
    let mut zeros = Vec::new();
    for (level, &side) in sides.iter().enumerate() {
        if level > 0 {
            let coarse = b.node(&format!("water_{level}"), "node.coarsen_water", Builder::lattice([side; 3], &[]));
            b.wire(water_levels[level - 1], coarse, "fine");
            water_levels.push((coarse, "out"));
            let open = b.node(&format!("solid_faces_{level}"), "node.coarsen_solid_faces", Builder::lattice([side; 3], &[]));
            b.wire(face_levels[level - 1], open, "fine");
            face_levels.push((open, "out"));
        }
        // The coarsest level is solved exactly by its inverse; the finest
        // also gives the zero rhs of −L p.
        if level == 0 || level + 1 < sides.len() {
            let zero = b.node(&format!("zero_{level}"), "node.zero_lattice", Builder::lattice([side; 3], &[]));
            zeros.push((zero, "out"));
        }
    }
    let coarsest = *sides.last().expect("a level");
    let inverse = b.node("coarse_inverse", "node.coarse_inverse", Builder::lattice([coarsest; 3], &[]));
    b.wire(*water_levels.last().expect("a level"), inverse, "water");
    b.wire(*face_levels.last().expect("a level"), inverse, "solid_faces");
    Levels { water: water_levels, faces: face_levels, zeros, inverse: (inverse, "out") }
}

/// One V-cycle for L e = rhs at `level`, from zero; returns e.
fn v_cycle(b: &mut Builder, s: PressureShape, levels: &Levels, level: usize, rhs: Port) -> Port {
    let sides = s.levels();
    let side = sides[level];
    let h = s.cell_size() * (1u64 << level) as f64;
    let lattice = |extra: &[(&str, Value)]| Builder::lattice([side; 3], extra);
    let water = levels.water[level];
    let faces = levels.faces[level];
    if level + 1 == sides.len() {
        // L = −A / h², so e = −h² · A⁻¹ · rhs; A⁻¹ is symmetric, so its rows
        // weighted by rhs are the product.
        let cells = side * side * side;
        let id = b.node(
            &format!("mg{level}_solve"),
            "node.combine_rows",
            json!({"row_length": int(cells), "rows": int(cells), "scale": float(-h * h), "base_scale": float(0.0)}),
        );
        b.wire(rhs, id, "base");
        b.wire(levels.inverse, id, "matrix");
        b.wire(rhs, id, "coef");
        return (id, "out");
    }
    let mut e = levels.zeros[level];
    let sweep = |b: &mut Builder, name: String, e: Port, color: usize| -> Port {
        let id = b.node(&name, "node.pressure_smooth", lattice(&[("cell_size", float(h)), ("color", int(color))]));
        b.wire(water, id, "water");
        b.wire(rhs, id, "rhs");
        b.wire(e, id, "value");
        b.wire(faces, id, "solid_faces");
        (id, "out")
    };
    for round in 0..SMOOTH_SWEEPS {
        for color in [0, 1] {
            e = sweep(b, format!("mg{level}_pre{round}_{color}"), e, color);
        }
    }
    let residual = b.node(&format!("mg{level}_residual"), "node.pressure_residual", lattice(&[("cell_size", float(h))]));
    b.wire(water, residual, "water");
    b.wire(rhs, residual, "rhs");
    b.wire(e, residual, "value");
    b.wire(faces, residual, "solid_faces");
    let restrict = b.node(&format!("mg{level}_restrict"), "node.restrict_lattice", Builder::lattice([sides[level + 1]; 3], &[]));
    b.wire((residual, "out"), restrict, "fine");
    b.wire(levels.water[level + 1], restrict, "water");
    let coarse = v_cycle(b, s, levels, level + 1, (restrict, "out"));
    let prolong = b.node(&format!("mg{level}_prolong"), "node.prolong_lattice", lattice(&[]));
    b.wire(e, prolong, "value");
    b.wire(coarse, prolong, "coarse");
    b.wire(water, prolong, "water");
    e = (prolong, "out");
    for round in 0..SMOOTH_SWEEPS {
        for color in [1, 0] {
            e = sweep(b, format!("mg{level}_post{round}_{color}"), e, color);
        }
    }
    e
}

/// One multigrid-preconditioned conjugate gradient solve of L p = f on the
/// water; returns the pressure. The loop body, per iteration: z = V-cycle(r),
/// β = r·z / (last r·z), p = z + β p, s = −L p, α = r·z / (p·s),
/// x = x − α p, r = r − α s.
fn solve(b: &mut Builder, s: PressureShape, levels: &Levels, f: Port) -> Port {
    let cells = s.cells();
    let h = s.cell_size();
    let cg = b.node("cg", "node.conjugate_gradient", json!({"iterations": int(s.iterations)}));
    b.wire(f, cg, "rhs");
    let r = (cg, "residual");
    let z = v_cycle(b, s, levels, 0, r);
    let rz = b.dot("rz", r, z, cells);
    let beta = b.divide("beta", rz, (cg, "rz"));
    let p = b.axpy("direction", z, (cg, "direction"), beta, 1.0, cells);
    let sp = b.node("minus_lp", "node.pressure_residual", Builder::lattice([s.n; 3], &[("cell_size", float(h))]));
    b.wire(levels.water[0], sp, "water");
    b.wire(levels.zeros[0], sp, "rhs");
    b.wire(p, sp, "value");
    b.wire(levels.faces[0], sp, "solid_faces");
    let sp = (sp, "out");
    let ps = b.dot("p_dot_s", p, sp, cells);
    let alpha = b.divide("alpha", rz, ps);
    let x = b.axpy("solution", (cg, "solution"), p, alpha, -1.0, cells);
    let r_next = b.axpy("residual", r, sp, alpha, -1.0, cells);
    b.wire(r_next, cg, "residual_in");
    b.wire(x, cg, "solution_in");
    b.wire(p, cg, "direction_in");
    b.wire(rz, cg, "rz_in");
    (cg, "solution")
}

/// Device bytes a scene holds inside the render graph at 1920×1080, as the
/// liquid extent check counts them: every array at the size the walk reached
/// plus what each node holds for itself. Textures are not counted.
#[cfg(test)]
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    tests::walk(&render_def(scene), false).expect("the rendered scene covers every dispatch").scene_bytes
}

/// CPU size proofs for every GPU FLIP graph, run before any GPU run of it: the
/// shared liquid extent rules (`liquid::extent`) at every lattice, bare,
/// meshed, rendered and frozen.
#[cfg(test)]
pub(super) mod tests {
    use ahash::AHashMap;

    use super::*;
    use crate::node_graph::liquid::extent::{AtomExtent, ExtentError, ExtentReport, ExtentRule, LIQUID_EXTENT_RULES, Verdict, check_graph};
    use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
    use crate::node_graph::validation::GraphError;
    use crate::node_graph::{EffectGraphDefExt, ExecutionPlan, Graph, PrimitiveRegistry, compile};

    fn registry() -> PrimitiveRegistry {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        registry
    }

    /// Test sources hold what the planner gives them; sinks read nothing.
    fn harness_node(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
        Ok(())
    }

    /// Fused regions are the freeze compiler's contract (BUG-2efy (fused
    /// output capacity probe)); the walk sizes what they read and write.
    fn rules(frozen: bool) -> Vec<ExtentRule> {
        let mut rules = LIQUID_EXTENT_RULES.to_vec();
        for type_id in ["test.value_source", "test.face_source", "test.value_sink", "test.liquid_sink", "test.mesh_sink"] {
            rules.push(ExtentRule { type_id, check: harness_node });
        }
        if frozen {
            rules.push(ExtentRule { type_id: "node.wgsl_compute", check: harness_node });
        }
        rules
    }

    fn built(def: &EffectGraphDef) -> (Graph, ExecutionPlan) {
        let graph = def.clone().into_graph(&registry(), &Default::default()).expect("the def builds");
        let plan = compile(&graph).expect("the def compiles");
        (graph, plan)
    }

    /// `def` walked by the liquid extent rules, frozen as the app renders it
    /// when `frozen`.
    pub(in crate::node_graph::primitives) fn walk(def: &EffectGraphDef, frozen: bool) -> Result<ExtentReport, ExtentError> {
        let (mut graph, plan) = if frozen {
            let view = crate::node_graph::freeze::install::fuse_generator_view(def, &registry()).expect("the def fuses");
            let graph = (*view.def).clone().into_graph(&registry(), &view.mesh_rules).expect("the fused def builds");
            let plan = compile(&graph).expect("the fused def compiles");
            (graph, plan)
        } else {
            built(def)
        };
        check_graph(&mut graph, &plan, &rules(frozen))
    }

    fn walked(def: &EffectGraphDef, frozen: bool, what: &str) -> ExtentReport {
        walk(def, frozen).unwrap_or_else(|error| panic!("{what}: {error}"))
    }

    /// Every lattice a scene may use, 16 to 256, the sides between the powers
    /// of two included. Each is proven here before any GPU run at it. 256
    /// holds the pool and column only with a lower fill: the Dam Break there
    /// places more particles than a count carries, and the domain refuses it
    /// by name (`gpu_flip_dam_break_past_the_count_rail_is_refused`).
    const LATTICES: [usize; 7] = [16, 24, 32, 48, 64, 96, 128];

    /// Every lattice at every iteration count of the iteration trend.
    #[test]
    fn gpu_flip_pressure_arrays_cover_every_dispatch() {
        for (n, iterations) in LATTICES.into_iter().chain([256]).flat_map(|n| TREND_ITERATIONS.map(|i| (n, i))) {
            let shape = PressureShape { iterations, ..PressureShape::at(n) };
            let def = pressure_def(shape);
            let (_, plan) = built(&def);
            assert_eq!(plan.substep_regions().len(), 1, "one conjugate gradient region");
            let report = walked(&def, false, &format!("pressure {n}³, {iterations} iterations"));
            let levels = shape.levels().len();
            assert!(report.checked >= 11 * levels, "checked only {} nodes at {n}³", report.checked);
        }
    }

    /// The V-cycle's levels: halved while every side is even and one is over
    /// 4, so the coarsest fits the exact solve at every lattice a scene uses.
    #[test]
    fn gpu_flip_levels_halve_to_the_exact_solve() {
        let sides = |n| PressureShape::at(n).levels();
        assert_eq!(sides(64), vec![64, 32, 16, 8, 4]);
        assert_eq!(sides(96), vec![96, 48, 24, 12, 6, 3]);
        for n in LATTICES.into_iter().chain([256]) {
            let coarsest = *sides(n).last().expect("a level");
            assert!((coarsest.pow(3) as u64) <= super::super::coarse_inverse::MAX_COARSE_CELLS, "{n}³ ends at {coarsest}³");
        }
    }

    /// Every running scene at every lattice, and the probes' variants, before
    /// any GPU run of it: the tick region's steps, their solves, the stats,
    /// the frame and the surface.
    #[test]
    fn gpu_flip_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::deep_pool, WaterScene::deep_drop, WaterScene::free_fall];
        let all = LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).flat_map(|scene| [scene, scene.with_surface()]);
        // The splash probes' scenes: four steps a tick is four copies of the
        // step.
        let refined = WaterScene::dam_break(128).with_surface();
        let bare = |n| WaterScene { spread_rate: 0.0, ..WaterScene::dam_break(n) };
        let step = WaterScene::dam_break(128);
        let probes = [
            refined.with_iterations(12),
            WaterScene { steps: 4, ..refined },
            bare(64),
            bare(128).with_surface(),
            step.with_iterations(4),
            WaterScene { density_once: false, ..WaterScene::dam_break(64) }.with_surface(),
            WaterScene { steps: 1, spread_rate: SPREAD_PER_STEP * 60.0, ..WaterScene::dam_break(64) }.with_surface(),
        ];
        for scene in all.chain(probes) {
            let n = scene.pressure.n;
            let def = water_def(scene);
            let (graph, plan) = built(&def);
            let regions = plan.substep_regions();
            assert_eq!(regions.len(), 1, "one tick region");
            assert_eq!(regions[0].inner.len(), scene.steps + scene.density_solves(), "one conjugate gradient region per solve, inside the tick");
            let report = walked(&def, false, &format!("scene {n}³, {} steps", scene.steps));
            assert!(report.checked > 40 * scene.steps, "checked only {} nodes at {n}³, {} steps", report.checked, scene.steps);
            let meshed = plan.steps().iter().any(|step| {
                graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
            });
            assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
        }
    }

    /// The tick region's body is the tick: every step copy and the stats, and
    /// nothing the frame or the domain runs once a frame.
    #[test]
    fn gpu_flip_tick_region_is_the_tick() {
        let scene = WaterScene::dam_break(64).with_surface();
        let (graph, plan) = built(&water_def(scene));
        let region = &plan.substep_regions()[0];
        let name = |step: usize| graph.get_node(plan.steps()[step].node).expect("plan node").node_id.as_str().to_string();
        assert_eq!(graph.get_node(region.boundary).expect("boundary").node_id.as_str(), "state");
        let body: Vec<String> = region.steps.iter().map(|&step| name(step)).collect();
        for k in 0..scene.steps {
            for node in ["sort", "water", "water_1", "faces", "gravity", "divergence", "project", "move", "cg"] {
                let node = format!("s{k}.{node}");
                assert!(body.contains(&node), "{node} is not in the tick");
            }
        }
        assert!(body.iter().any(|node| node == "s1.density.cg") && !body.iter().any(|node| node == "s0.density.cg"));
        assert!(body.iter().any(|node| node == "stats"), "the stats run every tick");
        let outside = ["domain", "fill", "solid", "frame", "initial_column"];
        assert!(!body.iter().any(|node| outside.contains(&node.as_str()) || node.starts_with("surface")), "{body:?}");
    }

    /// The rendered Dam Break's device bytes at every lattice, for the size
    /// ladder.
    #[test]
    fn gpu_flip_memory_at_every_lattice() {
        for n in LATTICES {
            for scale in [1, 2, 3] {
                let scene = WaterScene::dam_break(n).with_surface_scale(scale);
                let bytes = rendered_scene_bytes(scene);
                println!(
                    "GPU FLIP rendered Dam Break {n}³, surface scale {scale}: {} particles, {:.2} GB",
                    scene.particles(),
                    bytes as f64 / 1e9
                );
                assert!(bytes > 0);
            }
        }
    }

    /// A lattice whose coarsest level is past the exact solve is refused once,
    /// at build, naming the coarse inverse, never frame by frame: an odd side
    /// can't halve, and 80 halves only to 5³. The domain refuses the same
    /// Resolution by name first (the GPU FLIP conformance row).
    #[test]
    fn gpu_flip_refuses_an_illegal_lattice_at_build() {
        for n in [15, 63, 80, 81, 97] {
            let graph = pressure_def(PressureShape::at(n)).into_graph(&registry(), &Default::default()).expect("pressure def builds");
            match compile(&graph) {
                Err(GraphError::IllegalParams { node, reason }) => {
                    let kind = graph.get_node(node).expect("refused node exists").node.type_id().as_str().to_string();
                    assert!(kind == "node.coarse_inverse" && reason.contains("one workgroup"), "{n}³ refused by {kind}: {reason}");
                }
                other => panic!("{n}³ must be refused at build, got {:?}", other.map(|_| "a plan")),
            }
        }
    }

    /// Past the count a wire carries exactly, the domain refuses the Dam
    /// Break by name before any GPU work.
    #[test]
    fn gpu_flip_dam_break_past_the_count_rail_is_refused() {
        let scene = WaterScene::dam_break(128);
        let mut def = water_def(scene);
        let domain = def.nodes.iter_mut().find(|node| node.node_id.as_str() == "domain").expect("domain");
        for param in ["resolution", "built_resolution"] {
            domain.params.insert(param.into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: 256 });
        }
        match walk(&def, false) {
            Err(ExtentError::Refused { node, reason }) => {
                assert!(node.starts_with("domain") && reason.contains("Resolution") && reason.contains("Initial Fill Height"), "{node}: {reason}");
            }
            other => panic!("expected the domain to refuse 256³, got {other:?}"),
        }
    }

    /// The scenes as the render smoke runs them, inside the shipped render
    /// graph (`render_def`), at every lattice, and the shipped lattice at
    /// every Surface Detail. No card overwrites a def param at build, so the
    /// runtime runs the graph this walk checks.
    #[test]
    fn gpu_flip_rendered_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool];
        let coarser = LATTICES.into_iter().flat_map(|n| [1, 2].map(|scale| WaterScene::dam_break(n).with_surface_scale(scale)));
        let detail = (SURFACE_DETAIL_OFFSET..=SURFACE_DETAIL_OFFSET + 2).map(|scale| WaterScene::dam_break(64).with_surface_scale(scale));
        // The cadence probes: the density solve every step, one step a tick.
        let cadence = [
            WaterScene { density_once: false, ..WaterScene::dam_break(64) },
            WaterScene { steps: 1, spread_rate: SPREAD_PER_STEP * 60.0, ..WaterScene::dam_break(64) },
        ];
        // The published face grid, at every lattice.
        let faces = LATTICES.into_iter().map(|n| WaterScene::dam_break(n).with_faces());
        let registry = PrimitiveRegistry::with_builtin();
        for scene in LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).chain(coarser).chain(detail).chain(cadence).chain(faces) {
            let n = scene.pressure.n;
            let def = render_def(scene);
            let (_, plan) = built(&def);
            assert_eq!(plan.substep_regions()[0].inner.len(), scene.steps + scene.density_solves());
            let report = walked(&def, false, &format!("rendered {n}³"));
            assert!(report.checked > 40 * scene.steps, "checked only {} nodes at {n}³", report.checked);
            let runtime = crate::preset_runtime::PresetRuntime::from_def(def, &registry, None).expect("the rendered scene builds");
            let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
            assert!(shadowed.is_empty(), "{n}³ at surface scale {}: cards overwrite def params: {shadowed:?}", scene.surface_scale);
        }
    }

    /// The fill is the engine's: its site rule on the engine's boxes, read
    /// by the domain.
    #[test]
    fn gpu_flip_dam_break_fill_matches_the_engine_boxes() {
        let at64 = WaterScene::dam_break(64);
        assert_eq!((at64.pool_sites(), at64.box_sites()), (5, [[5, 43], [5, 67], [8, 120]]));
        assert_eq!(at64.particles(), 128 * 5 * 128 + 38 * 62 * 112);
        let at128 = WaterScene::dam_break(128);
        assert_eq!((at128.pool_sites(), at128.box_sites()), (10, [[10, 86], [10, 133], [16, 240]]));
        // The still pool is 1 m of floor and no box; the falling block is 1 m on a side.
        let pool = WaterScene::still_pool(64);
        assert_eq!((pool.pool_sites(), pool.particles()), (32, 128 * 32 * 128));
        let block = WaterScene::free_fall(64);
        assert_eq!((block.pool_sites(), block.particles()), (0, 32 * 32 * 32));
    }

    /// The conjugate gradient region holds exactly one iteration: one V-cycle
    /// (two sweep pairs down, the residual, the restriction, the coarse
    /// solve, the prolongation, two sweep pairs up) and the vector updates.
    /// The coarse water, the zero lattices and the coarse inverse are
    /// outside it.
    #[test]
    fn gpu_flip_pressure_region_is_one_iteration() {
        let (graph, plan) = built(&pressure_def(PressureShape::at(64)));
        let region = &plan.substep_regions()[0];
        let names: AHashMap<_, _> = graph.nodes().map(|n| (n.id, n.node_id.as_str().to_string())).collect();
        let mut body: Vec<String> = region.steps.iter().map(|&i| names[&plan.steps()[i].node].clone()).collect();
        body.sort();
        let vectors = ["cg", "rz", "beta", "direction", "minus_lp", "p_dot_s", "alpha", "solution", "residual"];
        let mut want: Vec<String> = vectors.iter().map(|s| (*s).to_string()).collect();
        for level in 0..4 {
            for round in 0..SMOOTH_SWEEPS {
                for color in [0, 1] {
                    want.push(format!("mg{level}_pre{round}_{color}"));
                    want.push(format!("mg{level}_post{round}_{color}"));
                }
            }
            for stage in ["residual", "restrict", "prolong"] {
                want.push(format!("mg{level}_{stage}"));
            }
        }
        want.push("mg4_solve".to_string());
        want.sort();
        assert_eq!(body, want);
        assert!(!body.contains(&"coarse_inverse".to_string()), "the inverse is built once per water lattice");
    }

    /// Each fused region of `def` as its members' node ids, `a + b`.
    fn fused_regions(def: &EffectGraphDef) -> Vec<String> {
        let report = crate::node_graph::fusion_report(def, &registry());
        let name = |id: u32| def.nodes.iter().find(|n| n.id == id).map_or("?".to_string(), |n| n.node_id.as_str().to_string());
        report.regions.iter().map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect::<Vec<_>>().join(" + ")).collect()
    }

    /// The frozen graphs at every lattice, before any GPU run of them: the
    /// running scene bare and meshed, and the render graph.
    #[test]
    fn gpu_flip_frozen_graphs_cover_every_dispatch() {
        for n in LATTICES {
            let scene = WaterScene::dam_break(n);
            for scene in [scene.with_surface(), scene.with_faces()] {
                let report = walked(&render_def(scene), true, &format!("frozen render {n}³"));
                assert!(report.checked > 40 * scene.steps, "checked only {} nodes at {n}³", report.checked);
            }
        }
    }

    /// The solve does not fuse: every lattice a sweep writes is gathered by
    /// the next (its neighbours) or fans out to several readers, and a
    /// buffer region has one output. The water step fuses one pair, the
    /// projection into the solids' constraint, whose fused kernel
    /// `gpu_flip_atom_tests::gpu_flip_projection_into_constraint_fuses`
    /// proves against the unfused one.
    #[test]
    fn gpu_flip_solve_and_step_do_not_fuse() {
        assert_eq!(fused_regions(&pressure_def(PressureShape::at(64))), Vec::<String>::new());
        assert_eq!(fused_regions(&water_def(WaterScene::dam_break(64))), vec!["s0.project + s0.constrain".to_string()]);
    }

    /// Nodes by id and wires sorted, so a hand edit's order does not count.
    fn canonical(def: &Value) -> Value {
        let mut def = def.clone();
        def["nodes"].as_array_mut().expect("nodes").sort_by_key(|node| node["id"].as_u64());
        def["wires"].as_array_mut().expect("wires").sort_by_key(|wire| {
            let end = |key: &str| wire[key].as_u64().expect("wire end");
            let port = |key: &str| wire[key].as_str().expect("wire port").to_string();
            (end("fromNode"), port("fromPort"), end("toNode"), port("toPort"))
        });
        def
    }

    /// The tick hands the state its last step's projected faces, extended by
    /// the face grid's valid layers, whatever the step count.
    #[test]
    fn gpu_flip_state_takes_the_last_steps_extended_faces() {
        for scene in [WaterScene::dam_break(64), WaterScene { steps: 1, ..WaterScene::dam_break(64) }] {
            let def = serde_json::to_value(water_def(scene)).expect("def");
            let name = |id: &Value| -> String {
                let nodes = def["nodes"].as_array().expect("nodes");
                nodes.iter().find(|n| n["id"] == *id).expect("wired node")["nodeId"].as_str().expect("name").to_string()
            };
            let wires = def["wires"].as_array().expect("wires");
            let into: Vec<_> = wires.iter().filter(|w| w["toPort"] == "faces_in").collect();
            assert_eq!(into.len(), 1, "one faces_in wire");
            assert_eq!(name(&into[0]["toNode"]), "state");
            assert_eq!(name(&into[0]["fromNode"]), format!("s{}.new_extend_{}", scene.steps - 1, scene.band_layers()));
        }
    }

    /// The CFL guard and the band at the lattices the extent walk covers, and
    /// the band the conformance row's face grid scene publishes.
    #[test]
    fn gpu_flip_band_follows_the_cfl_guard() {
        use crate::node_graph::liquid::conformance::{FACE_GRID_GPU_FLIP_LAYERS, FACE_GRID_RESOLUTION};
        assert_eq!(WaterScene::dam_break(FACE_GRID_RESOLUTION as usize).band_layers(), FACE_GRID_GPU_FLIP_LAYERS as usize);
        let at = |n: usize, steps: usize| {
            let s = WaterScene::dam_break(n).with_steps(steps);
            (s.travel_cells(), s.band_layers())
        };
        assert_eq!([at(64, 2), at(64, 1), at(128, 2), at(96, 2), at(16, 2)], [(3, 4), (6, 6), (6, 6), (4, 4), (1, 2)]);
    }

    /// The shipped `WaterDamBreakGpuFlip.json` is the builder's Dam Break at 64,
    /// so the tests that build it run what ships. `UPDATE_GPU_FLIP_PRESET=1`
    /// rewrites it from the builder.
    #[test]
    fn gpu_flip_shipped_preset_is_the_builders_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{SHIPPED_PRESET}.json"));
        let built = serde_json::to_value(render_def(WaterScene::dam_break(64).with_faces())).expect("serialise");
        if std::env::var("UPDATE_GPU_FLIP_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the shipped preset");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the shipped preset reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{SHIPPED_PRESET}.json differs from the builder's Dam Break; rerun with UPDATE_GPU_FLIP_PRESET=1");
    }

    /// The shipped preset loads, saves and reloads unchanged, the Whitewater
    /// group's params and its cards with it.
    #[test]
    fn gpu_flip_preset_round_trips_with_its_whitewater() {
        let shipped = shipped_preset();
        let def: EffectGraphDef = serde_json::from_value(shipped.clone()).expect("the preset loads");
        let loaded = serde_json::to_value(&def).expect("serialise");
        assert!(canonical(&loaded) == canonical(&shipped), "loading {SHIPPED_PRESET}.json dropped or changed a field");
        // A save prints each f32 at its shortest; the reload is the same f32s.
        let saved = serde_json::to_string_pretty(&def).expect("the preset saves");
        let again: EffectGraphDef = serde_json::from_str(&saved).expect("the saved preset reloads");
        let reloaded = serde_json::to_value(&again).expect("serialise");
        assert!(canonical(&reloaded) == canonical(&loaded), "a save and reload changed {SHIPPED_PRESET}.json");
        let nodes = reloaded["nodes"].as_array().expect("nodes");
        let group = nodes.iter().find(|n| n["nodeId"] == "whitewater").expect("the Whitewater group");
        for param in ["capacity", "wavecrest_emission", "min_energy", "max_energy"] {
            assert!(group["params"][param]["value"].is_number(), "the group lost {param}");
        }
        let cards = reloaded["presetMetadata"]["bindings"].as_array().expect("bindings");
        for card in ["whitewater_capacity", "foam_radius", "spray_radius", "bubble_density"] {
            assert!(cards.iter().any(|c| c["id"] == card), "the preset lost the {card} card");
        }
    }
}
