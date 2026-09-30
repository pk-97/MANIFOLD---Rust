//! The whitewater field against exact distances and FLIP
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7, O1), on the grid at 64:
//! 70³ cells over the 71³ solid lattice, the level set refined 3 times.
//! Spheres of radius 4, 8 and 16 cells with off-grid centres, and a sine
//! surface of amplitude 2 cells and wavelength 16.

use super::crossing_distance::CrossingDistance;
use super::liquid_surface_tests::{Harness, params};
use super::nearest_crossing::NearestCrossing;
use super::surface_crossings::SurfaceCrossings;
use super::whitewater_cpu::Grid;
use super::whitewater_grid_tests::run;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::whitewater::SurfaceCrossing;

const NODES: [u32; 3] = [71; 3];
/// The cell at 64: the 4 m tank over 64 cells.
const H: f32 = 4.0 / 64.0;
/// Surface Detail 3, the surface group's default.
const S: u32 = 3;

#[derive(Clone, Copy, Debug)]
enum Field {
    /// Liquid inside a ball; centre and radius in grid cells.
    Sphere { centre: [f64; 3], radius: f64 },
    /// Liquid below y = y0 + a·sin(2π(x − x0)/λ), extruded along z; in cells.
    Sine { y0: f64, x0: f64, amplitude: f64, wavelength: f64 },
}

const SPHERES: [Field; 3] = [
    Field::Sphere { centre: [35.37, 34.81, 35.58], radius: 4.0 },
    Field::Sphere { centre: [34.62, 35.29, 34.93], radius: 8.0 },
    Field::Sphere { centre: [35.13, 35.71, 34.44], radius: 16.0 },
];

const SINE: Field = Field::Sine { y0: 35.3, x0: 0.37, amplitude: 2.0, wavelength: 16.0 };

impl Field {
    /// Signed distance in cells at `p` (cells from the grid's first node),
    /// negative in the liquid.
    fn exact(&self, p: [f64; 3]) -> f64 {
        match *self {
            Field::Sphere { centre, radius } => (0..3).map(|a| (p[a] - centre[a]).powi(2)).sum::<f64>().sqrt() - radius,
            Field::Sine { y0, x0, amplitude, wavelength } => {
                let k = std::f64::consts::TAU / wavelength;
                let height = |t: f64| y0 + amplitude * (k * (t - x0)).sin();
                let slope = |t: f64| amplitude * k * (k * (t - x0)).cos();
                let curve = |t: f64| amplitude * k * k * -(k * (t - x0)).sin();
                let (x, y) = (p[0], p[1]);
                let vertical = y - height(x);
                // The nearest curve point lies within |vertical| of x: a
                // coarse scan, then Newton on d/dt |(t, height(t)) − p|².
                let reach = vertical.abs();
                let steps = ((2.0 * reach / 0.05).ceil() as usize).max(1);
                let mut best_t = x;
                let mut best = f64::INFINITY;
                for i in 0..=steps {
                    let t = x - reach + 2.0 * reach * i as f64 / steps as f64;
                    let dd = (t - x).powi(2) + (height(t) - y).powi(2);
                    if dd < best {
                        best = dd;
                        best_t = t;
                    }
                }
                let mut t = best_t;
                for _ in 0..8 {
                    let g = (t - x) + (height(t) - y) * slope(t);
                    let dg = 1.0 + slope(t).powi(2) + (height(t) - y) * curve(t);
                    if dg.abs() < 1e-12 {
                        break;
                    }
                    t -= g / dg;
                }
                let distance = ((t - x).powi(2) + (height(t) - y).powi(2)).sqrt().min(best.sqrt());
                distance.copysign(vertical)
            }
        }
    }

    fn radius(&self) -> Option<f64> {
        match *self {
            Field::Sphere { radius, .. } => Some(radius),
            Field::Sine { .. } => None,
        }
    }
}

fn centre_of(c: [u32; 3]) -> [f64; 3] {
    c.map(|v| f64::from(v) + 0.5)
}

fn grid_params(extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![("nodes_x", NODES[0] as f32), ("nodes_y", NODES[1] as f32), ("nodes_z", NODES[2] as f32)];
    all.extend_from_slice(extra);
    params(&all)
}

/// The value at quantile `q` of `values`.
fn quantile(values: &mut [f32], q: f64) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[((values.len() - 1) as f64 * q).round() as usize]
}

/// The field in particle_volume's capped form on the refined lattice: the
/// exact distance inside, capped at a tenth of a cell outside, the border
/// outside.
fn capped_level(field: Field, levels: [u32; 3]) -> Vec<f32> {
    let band = 0.1 * H;
    (0..levels.iter().product::<u32>())
        .map(|i| {
            let q = [i % levels[0], (i / levels[0]) % levels[1], i / (levels[0] * levels[1])];
            if (0..3).any(|a| q[a] == 0 || q[a] == levels[a] - 1) {
                return band;
            }
            let p = q.map(|v| f64::from(v) / f64::from(S));
            ((field.exact(p) * f64::from(H)) as f32).min(band)
        })
        .collect()
}

/// Crossings, three nearest passes and the distance, from the capped field
/// of spheres of radius 4, 8 and 16 cells: within a tenth of a cell of the
/// exact distance at every cell within 3 cells of the surface. Red as
/// measured: a crossing is a point on a refined edge, so a cell whose centre
/// sits on the surface still reads about 0.24 cell (0.707 of a refined edge
/// at 3 per cell), and a crossing spread from a neighbour sits up to a cell
/// to the side, 0.15–0.24 cell at 1–3 cells out. The curvature of that field
/// is off 2/r by 0.16–0.19 k·h at the median. BUG-7o8f (whitewater O1 red)
/// holds the decision.
#[test]
fn whitewater_redistance_matches_distance() {
    let grid = Grid::new(NODES);
    let levels = grid.cells.map(|c| c * S + 1);
    let solid = vec![10.0f32; grid.nodes.iter().product::<u32>() as usize];
    let mut harness = Harness::new();
    let solid_in = harness.array(&solid, solid.len());
    let level_params = grid_params(&[("level_nodes_x", levels[0] as f32), ("level_nodes_y", levels[1] as f32), ("level_nodes_z", levels[2] as f32)]);
    let mut failures = Vec::new();
    for field in SPHERES.into_iter().chain([SINE]) {
        let level = capped_level(field, levels);
        let level_in = harness.array(&level, level.len());
        let mut crossings: Vec<SurfaceCrossing> =
            run(&mut harness, &mut SurfaceCrossings::new(), &[("level_set", level_in.0), ("solid", solid_in.0)], grid.total(), &level_params);
        for _ in 0..3 {
            let input = harness.array(&crossings, crossings.len());
            crossings = run(&mut harness, &mut NearestCrossing::new(), &[("crossings", input.0)], grid.total(), &grid_params(&[]));
        }
        let crossings_in = harness.array(&crossings, crossings.len());
        let distance: Vec<f32> = run(
            &mut harness,
            &mut CrossingDistance::new(),
            &[("crossings", crossings_in.0), ("solid", solid_in.0)],
            grid.total(),
            &grid_params(&[("cell_size", H)]),
        );
        // Error by exact |d| in cells: [0, ¼), [¼, ½), [½, 1), [1, 2), [2, 3].
        let edges = [0.25, 0.5, 1.0, 2.0, 3.0];
        // [own crossing, propagated] per bucket.
        let mut worst = [[0.0f32; 5]; 2];
        let mut counts = [[0usize; 5]; 2];
        let mut over = 0usize;
        let mut worst_cell = ([0u32; 3], 0.0f32, 0.0f32);
        for (i, &phi) in distance.iter().enumerate() {
            let c = grid.coords(i);
            // The level set's border is outside (particle_volume closes the
            // surface there), so a field reaching the border has a second
            // surface this exact distance does not know.
            if c.iter().zip(grid.cells).any(|(&v, n)| v < 4 || v + 4 >= n) {
                continue;
            }
            let d = (field.exact(centre_of(c)) * f64::from(H)) as f32;
            if d.abs() > 3.0 * H {
                continue;
            }
            let error = (phi - d).abs() / H;
            let own = usize::from(!(0..3).all(|a| (crossings[i].crossing[a] - c[a] as f32).clamp(0.0, 1.0) == crossings[i].crossing[a] - c[a] as f32));
            let bucket = edges.iter().position(|&e| d.abs() / H < e).unwrap_or(4);
            worst[own][bucket] = worst[own][bucket].max(error);
            counts[own][bucket] += 1;
            if error > 0.1 {
                over += 1;
            }
            if error > worst_cell.2 {
                worst_cell = (c, d / H, error);
            }
        }
        println!(
            "{field:?}: by |d| bucket [<0.25, <0.5, <1, <2, <=3] cells; own-footprint crossing: counts {:?} worst {:?}; propagated: counts {:?} worst {:?}; {over} over 0.1, worst at {worst_cell:?}",
            counts[0], worst[0], counts[1], worst[1]
        );
        // What the emitter reads: curvature of this field, against 2/r.
        if let Some(radius) = field.radius() {
            let distance_in = harness.array(&distance, distance.len());
            let curvature: Vec<crate::node_graph::whitewater::KnownValue> = run(
                &mut harness,
                &mut super::lattice_curvature::LatticeCurvature::new(),
                &[("distance", distance_in.0)],
                grid.total(),
                &grid_params(&[("cell_size", H)]),
            );
            let mut errors: Vec<f32> = (0..grid.total())
                .filter(|&i| curvature[i].known > 0.0)
                .map(|i| {
                    let r = field.exact(centre_of(grid.coords(i))) + radius;
                    (f64::from(curvature[i].value * H) - 2.0 / r).abs() as f32
                })
                .collect();
            if !errors.is_empty() {
                let known = errors.len();
                println!("{field:?}: curvature of the re-distanced field over {known} known cells: p99 |kh - 2h/r| {:.4}, median {:.4}", quantile(&mut errors, 0.99), quantile(&mut errors, 0.5));
            }
        }
        if over > 0 {
            failures.push(format!("{field:?}: {over} cells over 0.1 cell, worst {worst_cell:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// FLIP's `calculateCurvatureGrid` and ours (curvature, then three
/// extension passes) on the same exact field: the 99th percentile of
/// |Δ(k·h)| within 0.05 over the nodes FLIP marks valid, and a sphere's mean
/// there within 5% of 2/R. Also held: our chain on FLIP's own reinitialised
/// field reproduces FLIP's grid, and ours on the exact field is within 0.05
/// of 2/r. Red as measured: FLIP reinitialises before its formula, and its
/// curvature of an exact sphere is off 2/r by 0.25/0.13/0.06 (R 4/8/16,
/// p99) where ours is off by 0.012/0.001/0.0001; BUG-7o8f (whitewater O1
/// red) holds the decision.
#[cfg(feature = "whitewater-oracle")]
#[test]
fn whitewater_curvature_matches_flip() {
    use super::extend_lattice::ExtendLattice;
    use super::lattice_curvature::LatticeCurvature;
    use crate::node_graph::whitewater::KnownValue;

    let grid = Grid::new(NODES);
    let total = grid.total();
    let mut harness = Harness::new();
    let mut failures = Vec::new();
    for field in SPHERES.into_iter().chain([SINE]) {
        let phi: Vec<f32> = (0..total).map(|i| (field.exact(centre_of(grid.coords(i))) * f64::from(H)) as f32).collect();
        let flip = manifold_fluids::whitewater_oracle::curvature(&phi, grid.cells, f64::from(H)).expect("FLIP curvature");
        let input = harness.array(&phi, total);
        let raw: Vec<KnownValue> = run(&mut harness, &mut LatticeCurvature::new(), &[("distance", input.0)], total, &grid_params(&[("cell_size", H)]));
        let mut extended = raw.clone();
        for _ in 0..3 {
            let input = harness.array(&extended, total);
            extended = run(&mut harness, &mut ExtendLattice::new(), &[("values", input.0)], total, &grid_params(&[]));
        }
        // FLIP's rule on its reinitialised field (particlelevelset.cpp:692).
        let near: Vec<bool> = flip.surface_phi.iter().map(|v| v.abs() < 2.0 * H).collect();
        let valid: Vec<usize> = (0..total)
            .filter(|&i| {
                let c = grid.coords(i);
                !grid.on_border(c) && near[i] && (0..6).all(|f| grid.face(c, f).is_some_and(|n| near[grid.index(n)]))
            })
            .collect();
        let mut deltas: Vec<f32> = valid.iter().map(|&i| ((extended[i].value - flip.curvature[i]) * H).abs()).collect();
        let unknown = valid.iter().filter(|&&i| raw[i].known == 0.0).count();
        let mean = valid.iter().map(|&i| f64::from(extended[i].value)).sum::<f64>() / valid.len().max(1) as f64;
        let flip_mean = valid.iter().map(|&i| f64::from(flip.curvature[i])).sum::<f64>() / valid.len().max(1) as f64;
        let p99 = quantile(&mut deltas, 0.99);
        let max = deltas.last().copied().unwrap_or(0.0);
        // Ours on FLIP's own reinitialised field: the same formula on the
        // same values, so the port is checked apart from FLIP's reinit.
        // The whole chain (validity, formula, three extension layers) on
        // FLIP's own reinitialised field, against FLIP's grid everywhere.
        let flip_field = harness.array(&flip.surface_phi, total);
        let mut on_flip: Vec<KnownValue> = run(&mut harness, &mut LatticeCurvature::new(), &[("distance", flip_field.0)], total, &grid_params(&[("cell_size", H)]));
        for _ in 0..3 {
            let input = harness.array(&on_flip, total);
            on_flip = run(&mut harness, &mut ExtendLattice::new(), &[("values", input.0)], total, &grid_params(&[]));
        }
        let identity_max = (0..total).map(|i| ((on_flip[i].value - flip.curvature[i]) * H).abs()).fold(0.0f32, f32::max);
        // Ours (where our rule computes it) and FLIP's against 2/r.
        let truth = |i: usize| field.radius().map(|radius| 2.0 / (field.exact(centre_of(grid.coords(i))) + radius));
        let mut ours_truth: Vec<f32> =
            valid.iter().filter(|&&i| raw[i].known > 0.0).filter_map(|&i| truth(i).map(|k| (f64::from(raw[i].value * H) - k).abs() as f32)).collect();
        let mut flip_truth: Vec<f32> = valid.iter().filter_map(|&i| truth(i).map(|k| (f64::from(flip.curvature[i] * H) - k).abs() as f32)).collect();
        let (ours_p99, flip_p99) = if ours_truth.is_empty() { (0.0, 0.0) } else { (quantile(&mut ours_truth, 0.99), quantile(&mut flip_truth, 0.99)) };
        println!(
            "{field:?}: {} valid, {unknown} of them unknown to ours, p99 |dkh| {p99:.4}, max {max:.4}, mean k·h ours {:.4} FLIP {:.4}; whole chain on FLIP's field max |dkh| {identity_max:.2e}; p99 |kh - 2h/r| ours {ours_p99:.4} FLIP {flip_p99:.4}",
            valid.len(),
            mean * f64::from(H),
            flip_mean * f64::from(H)
        );
        assert!(identity_max <= 1e-6, "{field:?}: our chain on FLIP's field is off FLIP's grid by {identity_max} in k·h");
        assert!(ours_p99 <= 0.05, "{field:?}: our curvature of the exact field is off 2/r by {ours_p99} in k·h (p99)");
        if valid.len() < 100 {
            failures.push(format!("{field:?}: only {} valid nodes", valid.len()));
        }
        if p99 > 0.05 {
            failures.push(format!("{field:?}: p99 |dkh| {p99}"));
        }
        if let Some(radius) = field.radius() {
            let want = 2.0 / (radius * f64::from(H));
            if (mean - want).abs() > 0.05 * want {
                failures.push(format!("{field:?}: mean {mean} against 2/R {want}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
