//! Checked against FLIP Fluids particlelevelset.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! The whitewater curvature chain against exact distances, on world tiles.
//! 70³ cells over the 71³ solid lattice, the box placed so it straddles
//! world tiles -1 and 0 on every axis.
//! Spheres of radius 4, 8 and 16 cells with off-grid centres, and a sine
//! surface of amplitude 2 cells and wavelength 16.

use super::extend_lattice::ExtendLattice;
use super::lattice_curvature::LatticeCurvature;
use super::whitewater_cpu::{Grid, canonical_surface};
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params};
use manifold_node_engine::testkit::fused_atoms::run;
use manifold_water_liquid::testkit::world_box::BoxTable;
use manifold_water_liquid::whitewater::KnownValue;

const NODES: [u32; 3] = [71; 3];
/// The cell at 64: the 4 m tank over 64 cells.
const H: f32 = 4.0 / 64.0;
/// The world cell of the box's first cell: 35 cells below the origin on every
/// axis, so the box crosses ten tiles a side, tiles -1 and 0 among them.
const ORIGIN: [i32; 3] = [-35; 3];

#[derive(Clone, Copy, Debug)]
enum Field {
    /// Liquid inside a ball; centre and radius in grid cells.
    Sphere { centre: [f64; 3], radius: f64 },
    /// Liquid below y = y0 + a·sin(2π(x − x0)/λ), extruded along z; in cells.
    Sine {
        y0: f64,
        x0: f64,
        amplitude: f64,
        wavelength: f64,
    },
}

const SPHERES: [Field; 3] = [
    Field::Sphere {
        centre: [35.37, 34.81, 35.58],
        radius: 4.0,
    },
    Field::Sphere {
        centre: [34.62, 35.29, 34.93],
        radius: 8.0,
    },
    Field::Sphere {
        centre: [35.13, 35.71, 34.44],
        radius: 16.0,
    },
];

const SINE: Field = Field::Sine {
    y0: 35.3,
    x0: 0.37,
    amplitude: 2.0,
    wavelength: 16.0,
};

impl Field {
    /// Signed distance in cells at `p` (cells from the grid's first node),
    /// negative in the liquid.
    fn exact(&self, p: [f64; 3]) -> f64 {
        match *self {
            Field::Sphere { centre, radius } => {
                (0..3)
                    .map(|a| (p[a] - centre[a]).powi(2))
                    .sum::<f64>()
                    .sqrt()
                    - radius
            }
            Field::Sine {
                y0,
                x0,
                amplitude,
                wavelength,
            } => {
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
                let distance = ((t - x).powi(2) + (height(t) - y).powi(2))
                    .sqrt()
                    .min(best.sqrt());
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

/// The value at quantile `q` of `values`.
fn quantile(values: &mut [f32], q: f64) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[((values.len() - 1) as f64 * q).round() as usize]
}

/// `phi` (metres, one per cell of `grid`) through node.lattice_curvature
/// and then `extend` passes of node.extend_lattice, on the pooled box at
/// `ORIGIN`; the box's cells (x fastest) read back out.
fn curvature_chain(
    harness: &mut Harness,
    grid: &Grid,
    phi: &[f32],
    extend: usize,
) -> Vec<KnownValue> {
    let nodes = BoxTable::new(ORIGIN, grid.nodes);
    let cells = nodes.with_cells(grid.cells);
    let len = pool_len(cells.pool_tiles) as usize;
    let tiles = harness.array(&cells.words, cells.words.len());
    let chain_params = params(&[("cell_size", H), ("pool_tiles", cells.pool_tiles as f32)]);
    let distance = harness.array(&cells.to_pool(phi, canonical_surface(H)), len);
    let mut values: Vec<KnownValue> = run(
        harness,
        &mut LatticeCurvature::new(),
        &[("distance", distance.0), ("tiles", tiles.0)],
        len,
        &chain_params,
    );
    for _ in 0..extend {
        let input = harness.array(&values, len);
        values = run(
            harness,
            &mut ExtendLattice::new(),
            &[("values", input.0), ("tiles", tiles.0)],
            len,
            &chain_params,
        );
    }
    cells.from_pool(&values)
}

/// Our curvature of each sphere's exact distance, over the cells its rule
/// knows, is within 0.05 of 2/r (p99, in k·h).
#[test]
fn whitewater_curvature_of_spheres_is_two_over_r() {
    let grid = Grid::new(NODES);
    let mut harness = Harness::new();
    for field in SPHERES {
        let radius = field.radius().expect("a sphere");
        let exact = |i: usize| field.exact(centre_of(grid.coords(i)));
        let phi: Vec<f32> = (0..grid.total())
            .map(|i| (exact(i) * f64::from(H)) as f32)
            .collect();
        let values = curvature_chain(&mut harness, &grid, &phi, 0);
        let mut error: Vec<f32> = (0..grid.total())
            .filter(|&i| values[i].known > 0.0)
            .map(|i| {
                let truth = 2.0 / (exact(i) + radius);
                (f64::from(values[i].value * H) - truth).abs() as f32
            })
            .collect();
        assert!(
            error.len() > 100,
            "{field:?}: only {} cells are known",
            error.len()
        );
        let p99 = quantile(&mut error, 0.99);
        assert!(
            p99 <= 0.05,
            "{field:?}: our curvature of the exact field is off 2/r by {p99} in k·h (p99)"
        );
    }
}

/// The sine surface is extruded along z, so its curvature and the three
/// extension layers after it are the same on every z slice at least four
/// cells from the box's z sides. A world tile face falls between every eight
/// slices, and none may change a value.
#[test]
fn whitewater_curvature_of_a_sine_surface_is_the_same_on_every_z_slice() {
    let grid = Grid::new(NODES);
    let [nx, ny, nz] = grid.cells.map(|n| n as usize);
    let slice: Vec<f32> = (0..nx * ny)
        .map(|i| {
            let c = [(i % nx) as u32, (i / nx) as u32, 0];
            (SINE.exact(centre_of(c)) * f64::from(H)) as f32
        })
        .collect();
    let phi: Vec<f32> = (0..grid.total()).map(|i| slice[i % (nx * ny)]).collect();
    let mut harness = Harness::new();
    let values = curvature_chain(&mut harness, &grid, &phi, 3);
    let at = |x: usize, y: usize, z: usize| values[x + nx * (y + ny * z)];
    let known = (0..nx * ny)
        .filter(|&i| at(i % nx, i / nx, 4).known > 0.0)
        .count();
    assert!(
        known > 300,
        "the sine's band holds {known} cells in a slice"
    );
    for z in 5..nz - 4 {
        for i in 0..nx * ny {
            let (x, y) = (i % nx, i / nx);
            let (a, b) = (at(x, y, 4), at(x, y, z));
            assert!(
                a.known == b.known && (a.value - b.value).abs() <= 1e-5 / H,
                "cell ({x}, {y}): slice 4 {a:?}, slice {z} {b:?}"
            );
        }
    }
}
