//! The FFT water pressure solve as one graph (docs/FFT_WATER_SOLVER_DESIGN.md
//! D3, D10, D11), built for any lattice so the 64³ fragment and its 128³ twin
//! come from one source. `water` and `f` are test array sources; `pressure`
//! is the result. `tests/fixtures/presets/fft_water_pressure.json` is this
//! graph at 64³, kept current by `fft_water_pressure_fragment_is_current`.

use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::{Value, json};

/// The box is 4 m on its longest side; the lowest wave it holds is 2π / 4 m.
pub(super) const BOX_METRES: f64 = 4.0;

/// Pass counts the GPU pass-count trend runs; the CPU size proof covers each.
pub(super) const TREND_PASSES: [usize; 4] = [12, 16, 24, 32];

#[derive(Clone, Copy, Debug)]
pub(super) struct PressureShape {
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

    /// Floats in the stacked chart planes: 6 views × sheets × n².
    pub fn planes(&self) -> usize {
        6 * self.sheets * self.n * self.n
    }
}

/// The FLIP Fluids engine's Dam Break (`WaterDamBreak.json`) with its
/// obstacle unwired: a 4 m cube over the floor, a 0.16 m pool, and the
/// `initial_column` block, seeded by the engine's half-cell site rule.
pub(super) const DAM_MIN: [f64; 3] = [-2.0, 0.0, -2.0];
pub(super) const DAM_FILL_HEIGHT: f64 = 0.16;
pub(super) const DAM_COLUMN: [[f64; 2]; 3] = [[-1.84, -0.66], [0.16, 2.08], [-1.75, 1.75]];

/// A liquid in the 4 m tank: a pool `fill_height` deep plus one box, both
/// in metres.
#[derive(Clone, Copy, Debug)]
pub(super) struct WaterScene {
    pub pressure: PressureShape,
    /// Water steps per frame (D8): copies of the step subgraph.
    pub steps: usize,
    pub flip: f64,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// How fast crowded cells spread (1/s): node.density_source's rate.
    pub spread_rate: f64,
}

/// Particles per cell the fill seeds: one per half-cell site.
pub(super) const REST_PER_CELL: f64 = 8.0;

/// The share of a cell's crowding the density source removes per step:
/// spread_rate × step dt. Crowding e decays as (1 − share) per step, so a
/// share over 1 overshoots and the water fizzes; the 64³ Dam Break sweep
/// (`fft_water_density_sweep`) shows it from 1.25 on. 5/6 is 100/s at two
/// steps per frame, the best measured rate under that bound.
pub(super) const SPREAD_PER_STEP: f64 = 5.0 / 6.0;

impl WaterScene {
    /// The engine's Dam Break, obstacle unwired.
    pub fn dam_break(n: usize) -> Self {
        let steps = 2;
        Self {
            pressure: PressureShape::at(n),
            steps,
            flip: 0.95,
            fill_height: DAM_FILL_HEIGHT,
            column: DAM_COLUMN,
            surface: false,
            spread_rate: SPREAD_PER_STEP * 60.0 * steps as f64,
        }
    }

    pub fn with_surface(self) -> Self {
        Self { surface: true, ..self }
    }

    /// Nodes per axis of the surface's solid lattice: the cell corners.
    pub fn surface_nodes(&self) -> usize {
        self.pressure.n + 1
    }

    /// A pool 1 m deep and nothing else (I5).
    pub fn still_pool(n: usize) -> Self {
        Self { fill_height: 1.0, column: [[0.0; 2]; 3], ..Self::dam_break(n) }
    }

    /// A 1 m block of water high in the tank, clear of every wall.
    pub fn free_fall(n: usize) -> Self {
        Self { fill_height: 0.0, column: [[-0.5, 0.5], [2.5, 3.5], [-0.5, 0.5]], ..Self::dam_break(n) }
    }

    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    fn range(&self, lo: f64, hi: f64, axis: usize) -> [u32; 2] {
        let s = self.pressure;
        super::liquid_fill::site_range(lo, hi, DAM_MIN[axis], s.cell_size(), s.n as u32)
    }

    pub fn pool_sites(&self) -> u32 {
        self.range(DAM_MIN[1], DAM_MIN[1] + self.fill_height, 1)[1]
    }

    pub fn box_sites(&self) -> [[u32; 2]; 3] {
        std::array::from_fn(|a| self.range(self.column[a][0], self.column[a][1], a))
    }

    /// Particles the fill places; the particle arrays hold exactly this many.
    pub fn particles(&self) -> u64 {
        super::liquid_fill::filled_sites([self.pressure.n as u32; 3], self.pool_sites(), self.box_sites())
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
pub(super) fn pressure_def(s: PressureShape) -> EffectGraphDef {
    let mut b = Builder::default();
    let cells = s.cells();
    let water = b.node("water", "test.value_source", json!({"max_capacity": int(cells)}));
    let f = b.node("f", "test.value_source", json!({"max_capacity": int(cells)}));
    let pressure = pressure(&mut b, s, (water, "out"), (f, "out"));
    let sink = b.node("sink", "test.value_sink", json!({}));
    b.wire(pressure, sink, "values");
    let output = b.node("output", "system.final_output", json!({}));
    b.wire((sink, "out"), output, "in");
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("pressure def")
}

/// A scene as a running liquid: the fill seeds a particle state, each frame
/// runs `steps` copies of the water step on it, and the last step's
/// particles become the next frame's state. `sink` holds the particles.
pub(super) fn water_def(scene: WaterScene) -> EffectGraphDef {
    let mut b = Builder::default();
    let s = scene.pressure;
    let sites = scene.box_sites();
    let fill = b.node(
        "fill",
        "node.liquid_fill",
        lattice_box(
            s,
            &[
                ("pool_sites", int(scene.pool_sites() as usize)),
                ("box_x0", int(sites[0][0] as usize)),
                ("box_x1", int(sites[0][1] as usize)),
                ("box_y0", int(sites[1][0] as usize)),
                ("box_y1", int(sites[1][1] as usize)),
                ("box_z0", int(sites[2][0] as usize)),
                ("box_z1", int(sites[2][1] as usize)),
                ("jitter", float(0.0)),
                ("seed", int(0)),
                ("max_capacity", int(scene.particles() as usize)),
            ],
        ),
    );
    let state = b.node("state", "node.liquid_feedback", json!({}));
    b.wire((fill, "particles"), state, "seed");
    let mut particles: Port = (state, "out");
    for k in 0..scene.steps {
        b.prefix = format!("s{k}.");
        particles = water_step(&mut b, scene, particles, (fill, "count"));
    }
    b.prefix.clear();
    b.wire(particles, state, "in");
    let output = b.node("output", "system.final_output", json!({}));
    let sink = if scene.surface {
        let mesh = surface(&mut b, scene, particles, (fill, "count"));
        let sink = b.node("mesh_sink", "test.mesh_sink", json!({}));
        b.wire(mesh, sink, "vertices");
        sink
    } else {
        let sink = b.node("sink", "test.liquid_sink", json!({}));
        b.wire(particles, sink, "particles");
        sink
    };
    b.wire((sink, "out"), output, "in");
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("water def")
}

/// The `liquid_surface` group of `WaterDamBreakGpu.json`, so SWASH is meshed
/// exactly as the shipped GPU surface meshes the engine's particles.
fn surface_group() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakGpu.json");
    let preset: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("preset reads")).expect("preset parses");
    let nodes = preset["nodes"].as_array().expect("preset nodes");
    nodes.iter().find(|node| node["nodeId"] == "liquid_surface").expect("liquid surface group").clone()
}

/// The liquid surface over the tank: its solid lattice is the cell corners,
/// with no solid in it (`solid` is a test source the harness zeroes).
fn surface(b: &mut Builder, scene: WaterScene, particles: Port, count: Port) -> Port {
    let nodes = scene.surface_nodes();
    let solid = b.node("solid", "test.value_source", json!({"max_capacity": int(nodes.pow(3))}));
    let half = 0.5 * BOX_METRES;
    let bounds = b.node(
        "tank",
        "node.transform_3d",
        json!({
            "pos_x": float(DAM_MIN[0] + half),
            "pos_y": float(DAM_MIN[1] + half),
            "pos_z": float(DAM_MIN[2] + half),
            "scale_x": float(BOX_METRES),
            "scale_y": float(BOX_METRES),
            "scale_z": float(BOX_METRES),
        }),
    );
    let corners = b.node("corners", "node.value", json!({"value": float(nodes as f64)}));
    let mut group = surface_group();
    let id = b.nodes.len();
    group["id"] = json!(id);
    group["nodeId"] = json!("surface");
    b.nodes.push(group);
    b.wire(particles, id, "particles");
    b.wire(count, id, "count");
    b.wire((solid, "out"), id, "solid");
    b.wire((bounds, "transform"), id, "bounds");
    for axis in ["nodes_x", "nodes_y", "nodes_z"] {
        b.wire((corners, "out"), id, axis);
    }
    (id, "vertices")
}

/// Lattice params plus the lattice's box: cell size and lowest corner.
fn lattice_box(s: PressureShape, extra: &[(&str, Value)]) -> Value {
    let mut params = Builder::lattice([s.n; 3], extra);
    params["cell_size"] = float(s.cell_size());
    for (axis, name) in ["lattice_min_x", "lattice_min_y", "lattice_min_z"].into_iter().enumerate() {
        params[name] = float(DAM_MIN[axis]);
    }
    params
}

/// One water step (section 3): sort, the water lattice, particles to faces,
/// gravity, the pressure solve, the projection, faces back to particles.
fn water_step(b: &mut Builder, scene: WaterScene, particles: Port, count: Port) -> Port {
    let s = scene.pressure;
    let n = [s.n; 3];
    let h = s.cell_size();
    let dt = scene.step_dt();
    let side = BOX_METRES;
    let sort = b.node(
        "sort",
        "node.sort_particles_into_cells",
        json!({
            "center_x": float(DAM_MIN[0] + 0.5 * side),
            "center_y": float(DAM_MIN[1] + 0.5 * side),
            "center_z": float(DAM_MIN[2] + 0.5 * side),
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
    let gather = b.node("faces", "node.particles_to_faces", lattice_box(s, &[]));
    b.wire((sort, "sorted"), gather, "sorted");
    b.wire((sort, "cell_ranges"), gather, "cell_ranges");
    let old = extend(b, "old", (gather, "out"), n);
    let forced = b.node("gravity", "node.face_gravity", Builder::lattice(n, &[("step_dt", float(dt))]));
    b.wire(old, forced, "faces");
    let divergence = b.node("divergence", "node.face_divergence", Builder::lattice(n, &[("cell_size", float(h))]));
    b.wire((forced, "out"), divergence, "faces");
    b.wire(water, divergence, "water");
    let spread = b.node(
        "density",
        "node.density_source",
        Builder::lattice(n, &[("rest", float(REST_PER_CELL)), ("rate", float(scene.spread_rate))]),
    );
    b.wire((divergence, "out"), spread, "divergence");
    b.wire((sort, "cell_ranges"), spread, "cell_ranges");
    let p = pressure(b, s, water, (spread, "out"));
    let projected = b.node("project", "node.subtract_pressure", Builder::lattice(n, &[("cell_size", float(h))]));
    b.wire((forced, "out"), projected, "faces");
    b.wire(p, projected, "pressure");
    b.wire(water, projected, "water");
    let new = extend(b, "new", (projected, "out"), n);
    let moved = b.node(
        "move",
        "node.faces_to_particles",
        lattice_box(s, &[("step_dt", float(dt)), ("flip", float(scene.flip))]),
    );
    b.wire((sort, "sorted"), moved, "particles");
    b.wire(new, moved, "faces");
    b.wire(old, moved, "old");
    (moved, "out")
}

/// Two layers of face extension into the air around the water.
fn extend(b: &mut Builder, name: &str, faces: Port, n: [usize; 3]) -> Port {
    let mut faces = faces;
    for layer in 1..=2 {
        let id = b.node(&format!("{name}_extend_{layer}"), "node.extend_faces", Builder::lattice(n, &[]));
        b.wire(faces, id, "faces");
        faces = (id, "out");
    }
    faces
}

/// The solve's nodes, from the water lattice and its divergence f; returns
/// the pressure.
fn pressure(b: &mut Builder, s: PressureShape, water: Port, f: Port) -> Port {
    let n = [s.n; 3];
    let cells = s.cells();

    // Setup: the collar list and each entry's place in the six views.
    let collar = b.node("collar", "node.collar_cells", Builder::lattice(n, &[]));
    b.wire(water, collar, "water");
    let total = b.node("collar_total", "node.running_total", json!({}));
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
    let charts = (charts, "out");

    // Right-hand side b = (G f at the collar, Σf / n³), β = |b|, start = b / β.
    let gf = b.box_solve("rhs_box", f, s);
    let sum_f = b.dots("sum_f", f, None, cells, 1, false);
    let rhs = b.node("rhs", "node.collar_gather", json!({}));
    b.wire((entries, "out"), rhs, "entries");
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
    b.wire((entries, "out"), w, "entries");
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
