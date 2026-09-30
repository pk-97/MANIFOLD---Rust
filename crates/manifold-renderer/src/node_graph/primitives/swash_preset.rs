//! The FFT water pressure solve as one graph (docs/FFT_WATER_SOLVER_DESIGN.md
//! D3, D10, D11), built for any lattice so the 64³ fragment and its 128³ twin
//! come from one source. `water` and `f` are test array sources; `pressure`
//! is the result. `tests/fixtures/presets/fft_water_pressure.json` is this
//! graph at 64³, kept current by `fft_water_pressure_fragment_is_current`.

use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::{Value, json};

/// The box is 4 m on its longest side; the lowest wave it holds is 2π / 4 m.
pub(super) const BOX_METRES: f64 = 4.0;

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

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn int(v: usize) -> Value {
    json!({"type": "Int", "value": v})
}

struct Builder {
    nodes: Vec<Value>,
    wires: Vec<Value>,
}

type Port = (usize, &'static str);

impl Builder {
    fn node(&mut self, name: &str, type_id: &str, params: Value) -> usize {
        let id = self.nodes.len();
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

/// The whole solve for one step: setup, right-hand side, the GMRES region,
/// and the pressure.
pub(super) fn pressure_def(s: PressureShape) -> EffectGraphDef {
    let mut b = Builder { nodes: Vec::new(), wires: Vec::new() };
    let n = [s.n; 3];
    let cells = s.cells();

    let water = b.node("water", "test.value_source", json!({"max_capacity": int(cells)}));
    let f = b.node("f", "test.value_source", json!({"max_capacity": int(cells)}));

    // Setup: the collar list and each entry's place in the six views.
    let collar = b.node("collar", "node.collar_cells", Builder::lattice(n, &[]));
    b.wire((water, "out"), collar, "water");
    let total = b.node("collar_total", "node.running_total", json!({}));
    b.wire((collar, "out"), total, "in");
    let total = (total, "out");
    let entries = b.node("entries", "node.select_flagged", json!({"capacity": int(s.capacity)}));
    b.wire(total, entries, "total");
    let mut smoothed: Port = (water, "out");
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
    b.wire((water, "out"), charts, "water");
    b.wire(smoothed, charts, "smoothed");
    b.wire((collar, "out"), charts, "collar");
    let charts = (charts, "out");

    // Right-hand side b = (G f at the collar, Σf / n³), β = |b|, start = b / β.
    let gf = b.box_solve("rhs_box", (f, "out"), s);
    let sum_f = b.dots("sum_f", (f, "out"), None, cells, 1, false);
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
    b.wire((water, "out"), pressure, "water");
    b.wire(gf, pressure, "solved");
    b.wire(correction, pressure, "correction");
    b.wire(lambda, pressure, "vector");
    let sink = b.node("sink", "test.value_sink", json!({}));
    b.wire((pressure, "out"), sink, "values");
    let output = b.node("output", "system.final_output", json!({}));
    b.wire((sink, "out"), output, "in");

    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("pressure def")
}
