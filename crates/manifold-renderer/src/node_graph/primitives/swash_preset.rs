//! SWASH, the FFT water solver (docs/FFT_WATER_SOLVER_DESIGN.md), as graphs
//! built for any lattice. `water_def` is a running liquid on the liquid seam
//! (docs/LIQUID_SOLVER_SEAM_DESIGN.md P7a): node.swash_domain's clock runs
//! node.liquid_state's tick region, whose body is one 60 Hz tick of
//! [`STEPS_PER_TICK`] water steps, the density solve on the last, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakSwash.json`, which is its own Dam Break at 64. The solver's
//! lattice is baked into its atoms' params, and the domain refuses any other
//! (lifted in P7b).

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::SWASH_DOMAIN_TYPE_ID;
use serde_json::{Value, json};

use super::swash_domain::{SwashGeometry, swash_geometry};
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
/// coupled body moves per step or per tick is SWASH P3b's to settle.
pub(crate) const STEPS_PER_TICK: usize = 2;

/// Pass counts the GPU pass-count trend runs; the CPU size proof covers each.
#[cfg(test)]
pub(super) const TREND_PASSES: [usize; 4] = [12, 16, 24, 32];

#[derive(Clone, Copy, Debug)]
pub(crate) struct PressureShape {
    /// Cells per side of the cubic lattice.
    pub n: usize,
    /// Collar entries the Krylov vectors hold (the constant is one more).
    pub capacity: usize,
    pub passes: usize,
    pub sheets: usize,
}

impl PressureShape {
    pub fn at(n: usize) -> Self {
        // The collar grows with the surface, n². The largest Dam Break collar
        // is 18,655 cells at 64³ and 94,154 at 128³ (refined); 8 n² holds both.
        Self { n, capacity: 8 * n * n, passes: 24, sheets: 4 }
    }

    pub fn cells(&self) -> usize {
        self.n * self.n * self.n
    }

    pub fn cell_size(&self) -> f64 {
        BOX_METRES / self.n as f64
    }

    pub fn row_length(&self) -> usize {
        self.capacity + 1
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
    pub flip: f64,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// How fast crowded cells spread (1/s): node.density_source's rate. 0
    /// leaves the density solve out.
    pub spread_rate: f64,
    /// Krylov passes of the density solve.
    pub density_passes: usize,
    /// Run the density solve on the tick's last step only. The spread rate
    /// stays per step, so that one solve removes the same share. On in the
    /// shipped cadence: at 64³ it holds the water measures within 1.5 points
    /// of a solve every step (`fft_water_cadence_64`) and saves one solve.
    pub density_once: bool,
    /// Surface lattice nodes per cell (`resolution_scale` of the surface's
    /// volume and mesh): the shipped Surface Detail 0 is 2, which fits the
    /// frame budget at 64 (BUG-mjhx, surface scale at SWASH 64).
    pub surface_scale: usize,
    /// Publish the face grid: three node.face_sample_component named
    /// [`FACE_NODES`] on the state's faces after the region, into the frame.
    /// The tick always hands its last step's faces to the state.
    pub faces: bool,
}

/// The face grid's nodes in a scene built with `faces`, x, y and z.
pub(crate) const FACE_NODES: [&str; 3] = ["face_u", "face_v", "face_w"];

/// Face layers past the water SWASH extends each step's faces by: the face
/// grid's `face_valid_layers`.
pub(crate) const EXTENDED_LAYERS: usize = 2;

/// Particles per cell the fill seeds: one per half-cell site.
pub(crate) const REST_PER_CELL: f64 = 8.0;

/// The share of a cell's crowding one density solve removes: spread_rate ×
/// step dt. Linear theory says crowding goes as (1 − share) per solve, so 1
/// removes it in one solve; particles are discrete, so an overshoot crowds
/// the next cell. Dam Break volume drift, max over the run, with a solve
/// every step (`fft_water_density_sweep`, `fft_water_refined_splash`): share
/// 1 gives 19.5% at 64³ and 9.0% at 128³; 5/6 gives 26.3% and 12.9%; 1.5
/// leaves twice the particles past rest at 128³ (32% against 16% at frame 29).
pub(crate) const SPREAD_PER_STEP: f64 = 1.0;

/// The density solve's passes. It moves particles and is never kept as
/// velocity, so its leftover error shows as a slightly uneven spread, not
/// as motion.
pub(crate) const DENSITY_PASSES: usize = 8;

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
            density_passes: DENSITY_PASSES,
            density_once: true,
            surface_scale: 2,
            faces: false,
        }
    }

    /// A pool 1 m deep and nothing else (I5).
    pub fn still_pool(n: usize) -> Self {
        Self { fill_height: 1.0, column: [[0.0; 2]; 3], ..Self::dam_break(n) }
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

    /// The same scene with `passes` Krylov passes per solve.
    #[cfg(test)]
    pub fn with_passes(self, passes: usize) -> Self {
        Self { pressure: PressureShape { passes, ..self.pressure }, ..self }
    }

    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
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
    /// lattice and the particle count, from node.swash_domain's function.
    pub fn geometry(&self) -> SwashGeometry {
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
        swash_geometry(read, &params, None, self.initial_volume()).expect("the scene fits its domain")
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

    /// Forward cosine transform, `middle`, inverse: the box solve (axes 3,
    /// middle cosine_poisson_divide) or the surface operator (axes 2, middle
    /// cosine_surface_scale).
    fn cosine_sandwich(&mut self, prefix: &str, input: Port, nodes: [usize; 3], axes: usize, middle: (&str, Value)) -> Port {
        let fwd = self.node(
            &format!("{prefix}_order"),
            "node.cosine_reorder",
            Self::lattice(nodes, &[("direction", int(0)), ("axes", int(axes))]),
        );
        self.wire(input, fwd, "values");
        let fft = self.node(&format!("{prefix}_fft"), "node.fft_3d", Self::lattice(nodes, &[("axes", int(axes))]));
        self.wire((fwd, "out"), fft, "values");
        let spectrum =
            self.node(&format!("{prefix}_cosine"), "node.cosine_spectrum", Self::lattice(nodes, &[("axes", int(axes))]));
        self.wire((fft, "spectrum"), spectrum, "spectrum");
        let scale = self.node(&format!("{prefix}_scale"), middle.0, middle.1);
        self.wire((spectrum, "out"), scale, "values");
        let half =
            self.node(&format!("{prefix}_half"), "node.cosine_half_spectrum", Self::lattice(nodes, &[("axes", int(axes))]));
        self.wire((scale, "out"), half, "values");
        let ifft =
            self.node(&format!("{prefix}_ifft"), "node.inverse_fft_3d", Self::lattice(nodes, &[("axes", int(axes))]));
        self.wire((half, "spectrum"), ifft, "spectrum");
        let back = self.node(
            &format!("{prefix}_unorder"),
            "node.cosine_reorder",
            Self::lattice(nodes, &[("direction", int(1)), ("axes", int(axes))]),
        );
        self.wire((ifft, "values"), back, "values");
        (back, "out")
    }

    fn box_solve(&mut self, prefix: &str, input: Port, s: PressureShape) -> Port {
        let n = [s.n; 3];
        let divide = Self::lattice(n, &[("cell_size", float(s.cell_size()))]);
        self.cosine_sandwich(prefix, input, n, 3, ("node.cosine_poisson_divide", divide))
    }

    /// The six-view surface helper applied to a collar vector.
    fn helper(&mut self, prefix: &str, total: Port, charts: Port, value: Port, s: PressureShape) -> Port {
        let n = [s.n; 3];
        let sums = self.node(
            &format!("{prefix}_sums"),
            "node.chart_sums",
            Self::lattice(n, &[("sheets", int(s.sheets))]),
        );
        self.wire(total, sums, "total");
        self.wire(charts, sums, "entries");
        self.wire(value, sums, "value");
        let h = s.cell_size();
        let planes = [s.n, s.n, 6 * s.sheets];
        let surface = Self::lattice(
            planes,
            &[
                ("cell_size", float(h)),
                ("lowest_wave", float(2.0 * std::f64::consts::PI / BOX_METRES)),
                ("offset", float(2.0 / h)),
            ],
        );
        let smoothed = self.cosine_sandwich(prefix, (sums, "out"), planes, 2, ("node.cosine_surface_scale", surface));
        let spread = self.node(
            &format!("{prefix}_spread"),
            "node.chart_spread",
            Self::lattice(n, &[("sheets", int(s.sheets)), ("cell_size", float(h))]),
        );
        self.wire(charts, spread, "entries");
        self.wire(smoothed, spread, "planes");
        self.wire(value, spread, "value");
        (spread, "out")
    }

    /// Row sums (`vector` unwired) or dot products against `vector`.
    fn dots(&mut self, name: &str, matrix: Port, vector: Option<Port>, row_length: usize, rows: usize, root: bool) -> Port {
        let id = self.node(
            name,
            "node.dot_products",
            json!({"row_length": int(row_length), "rows": int(rows), "max_rows": int(rows), "root": int(usize::from(root))}),
        );
        self.wire(matrix, id, "matrix");
        if let Some(vector) = vector {
            self.wire(vector, id, "vector");
        }
        (id, "out")
    }

    /// One Gram–Schmidt round: h = V·w over the first `rows` basis rows, then w − V h.
    fn project(&mut self, round: usize, w: Port, basis: Port, rows: Port, s: PressureShape) -> (Port, Port) {
        let dots = self.node(
            &format!("h{round}"),
            "node.dot_products",
            json!({"row_length": int(s.row_length()), "max_rows": int(s.passes + 1)}),
        );
        self.wire(basis, dots, "matrix");
        self.wire(w, dots, "vector");
        self.wire(rows, dots, "rows");
        let update = self.node(
            &format!("w{round}"),
            "node.combine_rows",
            json!({"row_length": int(s.row_length()), "scale": float(-1.0), "base_scale": float(1.0)}),
        );
        self.wire(w, update, "base");
        self.wire(basis, update, "matrix");
        self.wire((dots, "out"), update, "coef");
        self.wire(rows, update, "rows");
        ((dots, "out"), (update, "out"))
    }
}

/// The whole solve for one step, from the water lattice and its divergence:
/// setup, right-hand side, the GMRES region, and the pressure.
#[cfg(test)]
pub(super) fn pressure_def(s: PressureShape) -> EffectGraphDef {
    let mut b = Builder::default();
    let cells = s.cells();
    let water = b.node("water", "test.value_source", json!({"max_capacity": capacity(cells)}));
    let f = b.node("f", "test.value_source", json!({"max_capacity": capacity(cells)}));
    let pressure = pressure(&mut b, s, (water, "out"), (f, "out"));
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
        SWASH_DOMAIN_TYPE_ID,
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
        (particles, faces) = water_step(&mut b, scene, particles, count, domain, density);
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
    let frame = b.node("frame", "node.liquid_frame", json!({"face_valid_layers": int(EXTENDED_LAYERS)}));
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

/// The shipped SWASH Dam Break. Its render (camera, lights, environment,
/// tank, water material, tone map) and its Liquid Surface group are the one
/// source every builder scene takes them from; its water is the builder's
/// Dam Break at 64 (`fft_water_shipped_preset_is_the_builders_dam_break`).
pub(crate) const SHIPPED_PRESET: &str = "WaterDamBreakSwash";

fn shipped_preset() -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(SHIPPED_PRESET)).expect("the SWASH preset is bundled");
    serde_json::from_str(&json).expect("the SWASH preset parses")
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

/// One water step (section 3): sort, the water lattice, particles to faces,
/// the domain's gravity, the pressure solve, the projection, the density
/// solve on the same collar when `density`, faces back to particles.
/// Returns the moved particles and the step's projected, extended faces.
fn water_step(
    b: &mut Builder,
    scene: WaterScene,
    particles: Port,
    count: Port,
    domain: usize,
    density: bool,
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
    let old = extend(b, "old", (gather, "out"), n);
    let forced = b.node("gravity", "node.face_gravity", Builder::lattice(n, &[("step_dt", float(dt))]));
    b.wire(old, forced, "faces");
    b.wire((domain, "gravity_x"), forced, "gravity_x");
    b.wire((domain, "gravity"), forced, "gravity_y");
    b.wire((domain, "gravity_z"), forced, "gravity_z");
    let divergence = b.node("divergence", "node.face_divergence", Builder::lattice(n, &[("cell_size", float(h))]));
    b.wire((forced, "out"), divergence, "faces");
    b.wire(water, divergence, "water");
    let setup = collar(b, s, water);
    let p = solve(b, s, &setup, water, (divergence, "out"));
    let projected = subtract(b, "project", (forced, "out"), p, water, s);
    let new = extend(b, "new", projected, n);
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
        let q = solve(b, PressureShape { passes: scene.density_passes, ..s }, &setup, water, (crowding, "out"));
        let spread = subtract(b, "project", projected, q, water, s);
        let advect = extend(b, "advect", spread, n);
        b.prefix = outer;
        advect
    } else {
        new
    };
    let moved = b.node(
        "move",
        "node.faces_to_particles",
        lattice_box(&scene, &[("step_dt", float(dt)), ("flip", float(scene.flip))]),
    );
    b.wire((sort, "sorted"), moved, "particles");
    b.wire(new, moved, "faces");
    b.wire(old, moved, "old");
    b.wire(advect, moved, "advect");
    ((moved, "out"), new)
}

/// `faces` minus the gradient of `pressure` on the water's faces.
fn subtract(b: &mut Builder, name: &str, faces: Port, pressure: Port, water: Port, s: PressureShape) -> Port {
    let n = [s.n; 3];
    let id = b.node(name, "node.subtract_pressure", Builder::lattice(n, &[("cell_size", float(s.cell_size()))]));
    b.wire(faces, id, "faces");
    b.wire(pressure, id, "pressure");
    b.wire(water, id, "water");
    (id, "out")
}

/// [`EXTENDED_LAYERS`] layers of face extension into the air around the water.
fn extend(b: &mut Builder, name: &str, faces: Port, n: [usize; 3]) -> Port {
    let mut faces = faces;
    for layer in 1..=EXTENDED_LAYERS {
        let id = b.node(&format!("{name}_extend_{layer}"), "node.extend_faces", Builder::lattice(n, &[]));
        b.wire(faces, id, "faces");
        faces = (id, "out");
    }
    faces
}

/// The solve's nodes, from the water lattice and its divergence f; returns
/// the pressure.
#[cfg(test)]
fn pressure(b: &mut Builder, s: PressureShape, water: Port, f: Port) -> Port {
    let setup = collar(b, s, water);
    solve(b, s, &setup, water, f)
}

/// What every solve on one water lattice shares: the collar's running total,
/// its entries, and each entry's place in the six views.
#[derive(Clone, Copy)]
struct Collar {
    total: Port,
    entries: Port,
    charts: Port,
}

fn collar(b: &mut Builder, s: PressureShape, water: Port) -> Collar {
    let n = [s.n; 3];
    let collar = b.node("collar", "node.collar_cells", Builder::lattice(n, &[]));
    b.wire(water, collar, "water");
    // The capacity is the invariant check: a collar past it is named every
    // frame, never dropped silently.
    let total = b.node("collar_total", "node.running_total", json!({"capacity": int(s.capacity)}));
    b.wire((collar, "out"), total, "in");
    let total = (total, "out");
    let entries = b.node("entries", "node.select_flagged", json!({"capacity": int(s.capacity)}));
    b.wire(total, entries, "total");
    let mut smoothed: Port = water;
    for axis in 0..3 {
        let blur = b.node(
            &format!("smooth_{axis}"),
            "node.smooth_lattice",
            Builder::lattice(n, &[("passes", float(3.0)), ("axis", int(axis))]),
        );
        b.wire(smoothed, blur, "levelset");
        smoothed = (blur, "smoothed");
    }
    let charts = b.node("charts", "node.chart_entries", Builder::lattice(n, &[("sheets", int(s.sheets))]));
    b.wire((entries, "out"), charts, "entries");
    b.wire(water, charts, "water");
    b.wire(smoothed, charts, "smoothed");
    b.wire((collar, "out"), charts, "collar");
    Collar { total, entries: (entries, "out"), charts: (charts, "out") }
}

/// One solve on a set-up collar, for right-hand side f; returns the pressure.
fn solve(b: &mut Builder, s: PressureShape, setup: &Collar, water: Port, f: Port) -> Port {
    let cells = s.cells();
    let Collar { total, entries, charts } = *setup;

    // Right-hand side b = (G f at the collar, Σf / n³), β = |b|, start = b / β.
    let gf = b.box_solve("rhs_box", f, s);
    let sum_f = b.dots("sum_f", f, None, cells, 1, false);
    let rhs = b.node("rhs", "node.collar_gather", json!({}));
    b.wire(entries, rhs, "entries");
    b.wire(gf, rhs, "grid");
    b.wire(sum_f, rhs, "vector");
    b.wire(sum_f, rhs, "sum");
    let beta = b.dots("beta", (rhs, "out"), Some((rhs, "out")), s.row_length(), 1, true);
    let start = b.node("start", "node.divide_by_value", json!({}));
    b.wire((rhs, "out"), start, "values");
    b.wire(beta, start, "divisor");

    let solver = b.node(
        "krylov",
        "node.krylov_basis",
        json!({"passes": int(s.passes), "row_length": int(s.row_length())}),
    );
    b.wire(beta, solver, "seed");
    b.wire((start, "out"), solver, "start");
    let (current, basis, rows) = ((solver, "current"), (solver, "basis"), (solver, "rows"));

    // One pass: w = A · helper(current), two projection rounds, normalise, Givens.
    let z = b.helper("helper", total, charts, current, s);
    let source = b.node("pass_source", "node.collar_source", json!({}));
    b.wire(total, source, "total");
    b.wire(z, source, "value");
    let gz = b.box_solve("pass_box", (source, "out"), s);
    let sum_z = b.dots("sum_z", z, None, s.capacity, 1, false);
    let w = b.node("w", "node.collar_gather", json!({}));
    b.wire(entries, w, "entries");
    b.wire(gz, w, "grid");
    b.wire(z, w, "vector");
    b.wire(sum_z, w, "sum");
    let (h1, w1) = b.project(1, (w, "out"), basis, rows, s);
    let (h2, w2) = b.project(2, w1, basis, rows, s);
    let norm = b.dots("norm", w2, Some(w2), s.row_length(), 1, true);
    let next = b.node("next", "node.divide_by_value", json!({}));
    b.wire(w2, next, "values");
    b.wire(norm, next, "divisor");
    b.wire((next, "out"), solver, "next_in");
    let givens = b.node("givens", "node.krylov_givens", json!({"passes": int(s.passes)}));
    b.wire((solver, "out"), givens, "state");
    b.wire(h1, givens, "first");
    b.wire(h2, givens, "second");
    b.wire(norm, givens, "norm");
    b.wire((solver, "pass"), givens, "column");
    b.wire((givens, "out"), solver, "in");

    // After the passes: y, u = V y, λ = helper(u), p = G f − G Jᵀλ + c in water.
    let y = b.node("y", "node.krylov_solve", json!({"passes": int(s.passes)}));
    b.wire((solver, "out"), y, "state");
    let u = b.node(
        "u",
        "node.combine_rows",
        json!({"row_length": int(s.row_length()), "rows": int(s.passes), "scale": float(1.0), "base_scale": float(0.0)}),
    );
    b.wire(current, u, "base");
    b.wire(basis, u, "matrix");
    b.wire((y, "out"), u, "coef");
    let lambda = b.helper("final_helper", total, charts, (u, "out"), s);
    let final_source = b.node("final_source", "node.collar_source", json!({}));
    b.wire(total, final_source, "total");
    b.wire(lambda, final_source, "value");
    let correction = b.box_solve("final_box", (final_source, "out"), s);
    let pressure = b.node("pressure", "node.collar_pressure", json!({}));
    b.wire(water, pressure, "water");
    b.wire(gf, pressure, "solved");
    b.wire(correction, pressure, "correction");
    b.wire(lambda, pressure, "vector");
    (pressure, "out")
}

/// Device bytes a scene holds inside the render graph at 1920×1080, as the
/// liquid extent check counts them: every array at the size the walk reached
/// plus what each node holds for itself. Textures are not counted.
#[cfg(test)]
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    tests::walk(&render_def(scene), false).expect("the rendered scene covers every dispatch").scene_bytes
}

/// CPU size proofs for every SWASH graph, run before any GPU run of it: the
/// shared liquid extent rules (`liquid::extent`) at every lattice, bare,
/// meshed, rendered and frozen.
#[cfg(test)]
pub(super) mod tests {
    use ahash::AHashMap;

    use super::*;
    use crate::node_graph::liquid::extent::{AtomExtent, ExtentError, ExtentReport, ExtentRule, LIQUID_EXTENT_RULES, Verdict, check_graph};
    use crate::node_graph::primitives::cosine_spectrum::half_spectrum_len;
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

    /// A frozen graph's fused cosine pair: member 0 is the twiddle stage,
    /// gathering the half spectrum of its lattice and writing the lattice.
    /// Other fused regions (the liquid surface's) are the freeze compiler's
    /// contract (BUG-2efy (fused output capacity probe)).
    fn fused_cosine_pair(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
        if !x.params().contains_key("n0_axes") {
            return Ok(());
        }
        let nodes = ["n0_nodes_x", "n0_nodes_y", "n0_nodes_z"].map(|name| x.param(name, 0.0).round().max(0.0) as u32);
        x.covers("src_0", u64::from(half_spectrum_len(nodes)) * 8)?;
        x.covers("dst", nodes.iter().map(|&n| u64::from(n)).product::<u64>() * 4)
    }

    fn rules(frozen: bool) -> Vec<ExtentRule> {
        let mut rules = LIQUID_EXTENT_RULES.to_vec();
        for type_id in ["test.value_source", "test.value_sink", "test.liquid_sink", "test.mesh_sink"] {
            rules.push(ExtentRule { type_id, check: harness_node });
        }
        if frozen {
            rules.push(ExtentRule { type_id: "node.wgsl_compute", check: fused_cosine_pair });
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

    /// Every lattice a scene may use, 16 to 256, the mixed-radix sides between
    /// the powers of two included. Each is proven here before any GPU run at
    /// it. 256 holds the pool and column only with a lower fill: the Dam
    /// Break there places more particles than a count carries, and the domain
    /// refuses it by name (`fft_water_dam_break_past_the_count_rail_is_refused`).
    const LATTICES: [usize; 7] = [16, 32, 48, 64, 80, 96, 128];

    /// Every lattice at every pass count of the pass-count trend.
    #[test]
    fn fft_water_pressure_arrays_cover_every_dispatch() {
        for (n, passes) in LATTICES.into_iter().chain([256]).flat_map(|n| TREND_PASSES.map(|p| (n, p))) {
            let shape = PressureShape { passes, ..PressureShape::at(n) };
            let def = pressure_def(shape);
            let (_, plan) = built(&def);
            assert_eq!(plan.substep_regions().len(), 1, "one Krylov region");
            let report = walked(&def, false, &format!("pressure {n}³, {passes} passes"));
            assert!(report.checked > 60, "checked only {} nodes at {n}³", report.checked);
        }
    }

    /// Every running scene at every lattice, and the probes' variants, before
    /// any GPU run of it: the tick region's steps, their solves, the stats,
    /// the frame and the surface.
    #[test]
    fn fft_water_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::free_fall];
        let all = LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).flat_map(|scene| [scene, scene.with_surface()]);
        // The splash probes' scenes: the Krylov basis grows with passes, and
        // four steps a tick is four copies of the step.
        let refined = WaterScene::dam_break(128).with_surface();
        let bare = |n| WaterScene { spread_rate: 0.0, ..WaterScene::dam_break(n) };
        let step = WaterScene::dam_break(128);
        let probes = [
            refined.with_passes(16),
            refined.with_passes(32),
            WaterScene { steps: 4, ..refined },
            bare(64),
            bare(128).with_surface(),
            step.with_passes(32),
            step.with_passes(48),
            WaterScene { density_once: false, ..WaterScene::dam_break(64) }.with_surface(),
            WaterScene { steps: 1, spread_rate: SPREAD_PER_STEP * 60.0, ..WaterScene::dam_break(64) }.with_surface(),
        ];
        for scene in all.chain(probes) {
            let n = scene.pressure.n;
            let def = water_def(scene);
            let (graph, plan) = built(&def);
            let regions = plan.substep_regions();
            assert_eq!(regions.len(), 1, "one tick region");
            assert_eq!(regions[0].inner.len(), scene.steps + scene.density_solves(), "one Krylov region per solve, inside the tick");
            let report = walked(&def, false, &format!("scene {n}³, {} steps", scene.steps));
            assert!(report.checked > 70 * scene.steps, "checked only {} nodes at {n}³, {} steps", report.checked, scene.steps);
            let meshed = plan.steps().iter().any(|step| {
                graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
            });
            assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
        }
    }

    /// The tick region's body is the tick: every step copy and the stats, and
    /// nothing the frame or the domain runs once a frame.
    #[test]
    fn fft_water_tick_region_is_the_tick() {
        let scene = WaterScene::dam_break(64).with_surface();
        let (graph, plan) = built(&water_def(scene));
        let region = &plan.substep_regions()[0];
        let name = |step: usize| graph.get_node(plan.steps()[step].node).expect("plan node").node_id.as_str().to_string();
        assert_eq!(graph.get_node(region.boundary).expect("boundary").node_id.as_str(), "state");
        let body: Vec<String> = region.steps.iter().map(|&step| name(step)).collect();
        for k in 0..scene.steps {
            for node in ["sort", "water", "faces", "gravity", "divergence", "project", "move", "krylov"] {
                let node = format!("s{k}.{node}");
                assert!(body.contains(&node), "{node} is not in the tick");
            }
        }
        assert!(body.iter().any(|node| node == "s1.density.krylov") && !body.iter().any(|node| node == "s0.density.krylov"));
        assert!(body.iter().any(|node| node == "stats"), "the stats run every tick");
        let outside = ["domain", "fill", "solid", "frame", "initial_column"];
        assert!(!body.iter().any(|node| outside.contains(&node.as_str()) || node.starts_with("surface")), "{body:?}");
    }

    /// A pass count past the Krylov kernels' local arrays is refused at build,
    /// naming the Krylov node; it never runs as fewer passes.
    #[test]
    fn fft_water_refuses_passes_past_the_kernel_cap() {
        let scene = WaterScene::dam_break(64);
        let cap = super::super::krylov_givens::MAX_PASSES as usize;
        let build = |passes| water_def(scene.with_passes(passes)).into_graph(&registry(), &Default::default()).expect("water def builds");
        assert!(compile(&build(cap)).is_ok(), "{cap} passes compile");
        let graph = build(cap + 1);
        match compile(&graph) {
            Err(GraphError::IllegalParams { node, reason }) => {
                let kind = graph.get_node(node).expect("refused node exists").node.type_id().as_str().to_string();
                assert!(kind.contains("krylov") && reason.starts_with(&format!("passes {} ", cap + 1)), "refused by {kind}: {reason}");
            }
            other => panic!("{} passes must be refused at build, got {:?}", cap + 1, other.map(|_| "a plan")),
        }
    }

    /// The rendered Dam Break's device bytes at every lattice, for the size
    /// ladder.
    #[test]
    fn fft_water_memory_at_every_lattice() {
        for n in LATTICES {
            for scale in [1, 2, 3] {
                let scene = WaterScene::dam_break(n).with_surface_scale(scale);
                let bytes = rendered_scene_bytes(scene);
                println!(
                    "SWASH rendered Dam Break {n}³, surface scale {scale}: {} particles, {:.2} GB",
                    scene.particles(),
                    bytes as f64 / 1e9
                );
                assert!(bytes > 0);
            }
        }
    }

    /// What the collar capacity costs: the meshed Dam Break's bytes at
    /// today's 8n² and at the proven bound 6n³/7 (a collar cell is air beside
    /// water, and at most six air cells in seven can touch water).
    #[test]
    fn fft_water_collar_capacity_memory() {
        for n in [64, 128] {
            for capacity in [8 * n * n, 6 * n * n * n / 7] {
                let scene = WaterScene::dam_break(n).with_surface();
                let scene = WaterScene { pressure: PressureShape { capacity, ..scene.pressure }, ..scene };
                let report = walked(&water_def(scene), false, &format!("{n}³ capacity {capacity}"));
                println!("{n}³ capacity {capacity}: {:.0} MB", report.scene_bytes as f64 / 1e6);
            }
        }
    }

    /// A lattice the FFT atoms can't transform is refused once, at build,
    /// naming the transform, never frame by frame: an odd side can't pair the
    /// cosine reorder's nodes. The domain refuses the same Resolution by name
    /// first (the SWASH conformance row).
    #[test]
    fn fft_water_refuses_an_illegal_lattice_at_build() {
        for n in [63, 81, 97] {
            let graph = pressure_def(PressureShape::at(n)).into_graph(&registry(), &Default::default()).expect("pressure def builds");
            match compile(&graph) {
                Err(GraphError::IllegalParams { node, reason }) => {
                    let kind = graph.get_node(node).expect("refused node exists").node.type_id().as_str().to_string();
                    assert!(kind.contains("fft_3d") && reason.contains("even"), "{n}³ refused by {kind}: {reason}");
                }
                other => panic!("{n}³ must be refused at build, got {:?}", other.map(|_| "a plan")),
            }
        }
    }

    /// Past the count a wire carries exactly, the domain refuses the Dam
    /// Break by name before any GPU work.
    #[test]
    fn fft_water_dam_break_past_the_count_rail_is_refused() {
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
    fn fft_water_rendered_scenes_cover_every_dispatch() {
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
            assert!(report.checked > 70 * scene.steps, "checked only {} nodes at {n}³", report.checked);
            let runtime = crate::preset_runtime::PresetRuntime::from_def(def, &registry, None).expect("the rendered scene builds");
            let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
            assert!(shadowed.is_empty(), "{n}³ at surface scale {}: cards overwrite def params: {shadowed:?}", scene.surface_scale);
        }
    }

    /// The fill is the engine's: its site rule on the engine's boxes, read
    /// by the domain.
    #[test]
    fn fft_water_dam_break_fill_matches_the_engine_boxes() {
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

    /// The Krylov region holds exactly one pass: the helper, the box solve, the
    /// gather, two projection rounds, the norm and the Givens update.
    #[test]
    fn fft_water_pressure_region_is_one_pass() {
        let (graph, plan) = built(&pressure_def(PressureShape::at(64)));
        let region = &plan.substep_regions()[0];
        let names: AHashMap<_, _> = graph.nodes().map(|n| (n.id, n.node_id.as_str().to_string())).collect();
        let mut body: Vec<String> = region.steps.iter().map(|&i| names[&plan.steps()[i].node].clone()).collect();
        body.sort();
        let mut want: Vec<String> = [
            "krylov", "helper_sums", "helper_spread", "pass_source", "sum_z", "w", "h1", "w1", "h2", "w2", "norm",
            "next", "givens",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain(["helper", "pass_box"].iter().flat_map(|prefix| {
            ["order", "fft", "cosine", "scale", "half", "ifft", "unorder"].iter().map(move |s| format!("{prefix}_{s}"))
        }))
        .collect();
        want.sort();
        assert_eq!(body, want);
    }

    /// Each fused region of `def` as its members' node ids, `a + b`.
    fn fused_regions(def: &EffectGraphDef) -> Vec<String> {
        let report = crate::node_graph::fusion_report(def, &registry());
        let name = |id: u32| def.nodes.iter().find(|n| n.id == id).map_or("?".to_string(), |n| n.node_id.as_str().to_string());
        report.regions.iter().map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect::<Vec<_>>().join(" + ")).collect()
    }

    /// The frozen graphs at every lattice, before any GPU run of them: the
    /// solve alone, the running scene bare and meshed, and the render graph.
    #[test]
    fn fft_water_frozen_graphs_cover_every_dispatch() {
        for n in LATTICES {
            let frozen = crate::node_graph::freeze::install::fuse_generator_view(&pressure_def(PressureShape::at(n)), &registry())
                .expect("the solve fuses");
            let fused = frozen.def.nodes.iter().filter(|node| node.type_id == "node.wgsl_compute").count();
            assert_eq!(fused, 5, "the solve's five cosine pairs at {n}³");
            assert!(walked(&pressure_def(PressureShape::at(n)), true, "frozen solve").checked > 50);
            let scene = WaterScene::dam_break(n);
            for scene in [scene, scene.with_surface()] {
                let report = walked(&water_def(scene), true, &format!("frozen scene {n}³"));
                assert!(report.checked > 70 * scene.steps, "checked only {} nodes at {n}³", report.checked);
            }
            assert!(walked(&render_def(scene), true, &format!("frozen render {n}³")).checked > 140);
        }
    }

    /// The regions one solve fuses: every cosine transform's twiddle stage
    /// folds into what reads its lattice-sized output, the box's eigenvalue
    /// divide or the helper's plane scale.
    fn solve_regions(prefix: &str) -> Vec<String> {
        ["rhs_box", "helper", "pass_box", "final_helper", "final_box"]
            .iter()
            .map(|stage| format!("{prefix}{stage}_cosine + {prefix}{stage}_scale"))
            .collect()
    }

    /// The solve's fused regions; `fft_water_frozen_solve_matches_unfrozen`
    /// proves them on the GPU.
    #[test]
    fn fft_water_pressure_fused_regions() {
        let mut fused = fused_regions(&pressure_def(PressureShape::at(64)));
        let mut want = solve_regions("");
        fused.sort();
        want.sort();
        assert_eq!(fused, want);
    }

    /// The water step's fused regions: the solves' pairs and nothing else.
    /// cells_with_particles → face_divergence stays apart, because the water
    /// lattice fans out to the collar, the projections and the density source,
    /// and a buffer region has one output. Every other edge ends at a gather,
    /// crosses a solve or leaves the tick. `fft_water_frozen_step_matches_unfrozen`
    /// proves them.
    #[test]
    fn fft_water_step_fused_regions() {
        let scene = WaterScene::dam_break(64);
        let mut fused = fused_regions(&water_def(scene));
        let mut want: Vec<String> = ["s0.", "s1.", "s1.density."].iter().flat_map(|p| solve_regions(p)).collect();
        fused.sort();
        want.sort();
        assert_eq!(fused, want, "density once a tick, on the last step");
    }

    /// `tests/fixtures/presets/fft_water_pressure.json` is the 64³ graph.
    /// `UPDATE_SWASH_FRAGMENT=1` rewrites it.
    #[test]
    fn fft_water_pressure_fragment_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/presets/fft_water_pressure.json");
        let mut want = serde_json::to_string_pretty(&pressure_def(PressureShape::at(64))).expect("serialise");
        want.push('\n');
        if std::env::var("UPDATE_SWASH_FRAGMENT").is_ok() {
            std::fs::write(&path, &want).expect("write fragment");
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(have == want, "fft_water_pressure.json is stale; rerun with UPDATE_SWASH_FRAGMENT=1");
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
    fn fft_water_state_takes_the_last_steps_extended_faces() {
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
            assert_eq!(name(&into[0]["fromNode"]), format!("s{}.new_extend_{EXTENDED_LAYERS}", scene.steps - 1));
        }
    }

    /// The shipped `WaterDamBreakSwash.json` is the builder's Dam Break at 64,
    /// so the tests that build it run what ships. `UPDATE_SWASH_PRESET=1`
    /// rewrites it from the builder.
    #[test]
    fn fft_water_shipped_preset_is_the_builders_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{SHIPPED_PRESET}.json"));
        let built = serde_json::to_value(render_def(WaterScene::dam_break(64).with_faces())).expect("serialise");
        if std::env::var("UPDATE_SWASH_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the shipped preset");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the shipped preset reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{SHIPPED_PRESET}.json differs from the builder's Dam Break; rerun with UPDATE_SWASH_PRESET=1");
    }

    /// The shipped preset loads, saves and reloads unchanged, the Whitewater
    /// group's params and its cards with it.
    #[test]
    fn swash_preset_round_trips_with_its_whitewater() {
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
