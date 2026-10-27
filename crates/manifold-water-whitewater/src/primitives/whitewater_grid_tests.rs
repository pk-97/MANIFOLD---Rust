//! GPU value proofs for the whitewater grid atoms against their CPU
//! statements (`whitewater_cpu`), on world tiles. Each dense box of cells is
//! a pooled field under the table of the tiles it touches, its first cell at
//! world cell `ORIGIN` below the origin on x so the box straddles world tiles
//! -1 and 0; everything the pool holds outside the box is the canonical
//! element.

use super::extend_lattice::ExtendLattice;
use super::lattice_curvature::LatticeCurvature;
use super::turbulence_field::TurbulenceField;
use super::whitewater_cpu::{self as cpu, Grid, Rng};
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::scene::world_tiles::{pool_len, tile_of_cell};
use manifold_node_engine::testkit::array_harness::{Harness, params};
use manifold_node_engine::testkit::fused_atoms::run;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::liquid_cells::LiquidCells;
use manifold_water_liquid::testkit::world_box::{BoxTable, from_pool_under, table_words_of};
use manifold_water_liquid::whitewater::KnownValue;

/// Unequal sides, so a swapped axis shows.
const NODES: [u32; 3] = [9, 8, 7];
const H: f32 = 0.25;
/// The world cell of a box's first cell: x from -6, so the box is in tile -1
/// for its first six cells and tile 0 for the rest.
const ORIGIN: [i32; 3] = [-6, 0, 0];

/// The table of the box of `grid`'s solid nodes at `ORIGIN`, and the same
/// placement for its cells: node arrays go on the first, cell arrays on the
/// second, both under one table.
fn tables(grid: &Grid) -> (BoxTable, BoxTable) {
    let nodes = BoxTable::new(ORIGIN, grid.nodes);
    let cells = nodes.with_cells(grid.cells);
    (nodes, cells)
}

fn grid_params(pool_tiles: u32) -> ParamValues {
    params(&[("cell_size", H), ("pool_tiles", pool_tiles as f32)])
}

/// Elements of every pooled array of a pool of `table`'s tiles.
fn elements(table: &BoxTable) -> usize {
    pool_len(table.pool_tiles) as usize
}

/// A sphere of liquid with a little noise, in metres, sampled at `position`
/// given in grid cells.
fn sphere_at(position: [f32; 3], centre: [f32; 3], radius: f32, rng: &mut Rng) -> f32 {
    let r = (0..3)
        .map(|a| (position[a] - centre[a]).powi(2))
        .sum::<f32>()
        .sqrt();
    (r - radius + 0.1 * (rng.unit() - 0.5)) * H
}

/// A wall filling x < 1.3 cells, as a distance in metres on the solid nodes.
fn wall_solid(grid: &Grid) -> Vec<f32> {
    let n = grid.nodes;
    (0..n.iter().product::<u32>())
        .map(|i| ((i % n[0]) as f32 - 1.3) * H)
        .collect()
}

/// Air, liquid and solid, with the liquid shrunk off the air. The box's own
/// outer cells shrink too: the canonical neighbour beyond them is air.
#[test]
fn liquid_cells_match_cpu() {
    let grid = Grid::new(NODES);
    let (nodes, cells) = tables(&grid);
    let mut rng = Rng(0x11c0_e115);
    let distance: Vec<f32> = (0..grid.total())
        .map(|_| {
            if rng.unit() < 0.7 {
                -rng.unit()
            } else {
                rng.unit()
            }
        })
        .collect();
    let solid = wall_solid(&grid);
    let mut harness = Harness::new();
    let len = elements(&nodes);
    let (distance_in, solid_in, tiles) = (
        harness.array(&cells.to_pool(&distance, cpu::canonical_distance(H)), len),
        harness.array(&nodes.to_pool(&solid, cpu::CANONICAL_SOLID), len),
        harness.array(&nodes.words, nodes.words.len()),
    );
    let pooled: Vec<u32> = run(
        &mut harness,
        &mut LiquidCells::new(),
        &[
            ("distance", distance_in.0),
            ("solid", solid_in.0),
            ("tiles", tiles.0),
        ],
        len,
        &grid_params(nodes.pool_tiles),
    );
    let got = cells.from_pool(&pooled);
    let mut counts = [0usize; 3];
    for (i, &g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::liquid_cell(&grid, &distance, &solid, c);
        counts[want as usize] += 1;
        assert_eq!(g, want, "cell {c:?}");
    }
    let shrunk = (0..grid.total())
        .filter(|&i| distance[i] < 0.0 && got[i] == 0)
        .count();
    assert!(
        counts.iter().all(|&n| n > 10) && shrunk > 10,
        "kinds {counts:?}, {shrunk} shrunk"
    );
    // Every element the box leaves out is canonical: air.
    assert_eq!(
        pooled.iter().filter(|&&k| k != 0).count(),
        counts[1] + counts[2],
        "the pool holds liquid or wall outside the box"
    );
}

/// A sphere's distance on a box of `grid`'s cells, as the curvature fixture.
fn curvature_field(grid: &Grid) -> Vec<f32> {
    let mut rng = Rng(0xc0e7_0001);
    (0..grid.total())
        .map(|i| sphere_at(Grid::centre(grid.coords(i)), [6.2, 5.4, 4.9], 3.1, &mut rng))
        .collect()
}

/// `node.lattice_curvature` over the pooled distance `pool`, read through the
/// table `words` of `pool_tiles` slots.
fn curvature(
    harness: &mut Harness,
    pool: &[f32],
    words: &[u32],
    pool_tiles: u32,
) -> Vec<KnownValue> {
    let input = harness.array(pool, pool.len());
    let tiles = harness.array(words, words.len());
    run(
        harness,
        &mut LatticeCurvature::new(),
        &[("distance", input.0), ("tiles", tiles.0)],
        pool.len(),
        &grid_params(pool_tiles),
    )
}

/// FLIP's curvature where the band holds, unknown elsewhere, across the
/// tiles -1 and 0 on x and the absent tiles below the box on y and z.
#[test]
fn lattice_curvature_matches_cpu() {
    let grid = Grid::new([13, 12, 11]);
    let (_, cells) = tables(&grid);
    let distance = curvature_field(&grid);
    let pool = cells.to_pool(&distance, cpu::canonical_surface(H));
    let mut harness = Harness::new();
    let pooled = curvature(&mut harness, &pool, &cells.words, cells.pool_tiles);
    let got = cells.from_pool(&pooled);
    let mut known = 0;
    for (i, g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::lattice_curvature(&grid, &distance, H, c);
        known += usize::from(want.known > 0.0);
        assert_eq!(g.known, want.known, "cell {c:?}");
        assert!(
            (g.value - want.value).abs() <= 1e-4 / H,
            "cell {c:?}: GPU {} CPU {}",
            g.value,
            want.value
        );
    }
    assert!(
        known > 50 && known < grid.total() / 2,
        "{known} cells known"
    );
    // The canonical distance round the box is outside the band: no cell
    // there is known.
    assert_eq!(
        pooled.iter().filter(|v| v.known > 0.0).count(),
        known,
        "the pool knows a cell outside the box"
    );
}

/// A tile the table does not hold reads the canonical distance, and its
/// slot, left free, reads as nothing: dropping the tile that holds most of
/// the sphere leaves the cells there unknown, and the cells round it see the
/// canonical distance across the missing tile, though the pool still holds
/// the dropped tile's values in its slot.
#[test]
fn lattice_curvature_reads_canonical_where_no_tile_is_held() {
    let grid = Grid::new([13, 12, 11]);
    let (_, cells) = tables(&grid);
    let distance = curvature_field(&grid);
    let canon = cpu::canonical_surface(H);
    let dropped = [0, 0, 0];
    let held: Vec<_> = cells
        .held()
        .into_iter()
        .filter(|&(tile, _)| tile != dropped)
        .collect();
    assert_eq!(held.len() + 1, cells.pool_tiles as usize);
    let words = table_words_of(cells.pool_tiles, &held);
    let mut harness = Harness::new();
    let pooled = curvature(
        &mut harness,
        &cells.to_pool(&distance, canon),
        &words,
        cells.pool_tiles,
    );
    let got = from_pool_under(&pooled, &words, cells.pool_tiles, ORIGIN, grid.cells);
    // The distance as the table presents it.
    let masked: Vec<f32> = (0..grid.total())
        .map(|i| {
            let c = grid.coords(i);
            let tile = tile_of_cell(std::array::from_fn(|a| ORIGIN[a] + c[a] as i32));
            if tile == dropped { canon } else { distance[i] }
        })
        .collect();
    let (mut known, mut whole) = (0, 0);
    for (i, g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::lattice_curvature(&grid, &masked, H, c);
        known += usize::from(want.known > 0.0);
        whole += usize::from(cpu::lattice_curvature(&grid, &distance, H, c).known > 0.0);
        assert_eq!(g.known, want.known, "cell {c:?}");
        assert!(
            (g.value - want.value).abs() <= 1e-4 / H,
            "cell {c:?}: GPU {} CPU {}",
            g.value,
            want.value
        );
    }
    assert!(
        known > 10 && known + 20 < whole,
        "{known} cells known with the tile dropped, {whole} with it held"
    );
}

/// Three passes, each against the CPU pass on the same input.
#[test]
fn extend_lattice_matches_cpu() {
    let grid = Grid::new(NODES);
    let (_, cells) = tables(&grid);
    let mut rng = Rng(0xe7e7_0001);
    let mut values: Vec<KnownValue> = (0..grid.total())
        .map(|_| KnownValue {
            value: rng.unit() * 2.0 - 1.0,
            known: if rng.unit() < 0.1 { 1.0 } else { 0.0 },
        })
        .collect();
    let mut harness = Harness::new();
    let len = elements(&cells);
    let tiles = harness.array(&cells.words, cells.words.len());
    for pass in 0..3 {
        let input = harness.array(&cells.to_pool(&values, KnownValue::default()), len);
        let pooled: Vec<KnownValue> = run(
            &mut harness,
            &mut ExtendLattice::new(),
            &[("values", input.0), ("tiles", tiles.0)],
            len,
            &grid_params(cells.pool_tiles),
        );
        let got = cells.from_pool(&pooled);
        let want: Vec<KnownValue> = (0..grid.total())
            .map(|i| cpu::extend_lattice(&grid, &values, grid.coords(i)))
            .collect();
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                g.known == w.known && (g.value - w.value).abs() <= 1e-6,
                "pass {pass} cell {:?}: GPU {g:?} CPU {w:?}",
                grid.coords(i)
            );
        }
        values = want;
    }
    let known = values.iter().filter(|v| v.known > 0.0).count();
    assert!(
        known > grid.total() / 2,
        "three passes fill {known} of the {} cells",
        grid.total()
    );
}

/// FLIP's turbulence at each liquid cell from the pooled faces: cell-centre
/// velocities over the i-2 through i+1 window, reaching face records outside
/// the box (no weight, so no velocity) and cells with unweighted faces.
#[test]
fn turbulence_field_matches_cpu() {
    let grid = Grid::new(NODES);
    let (nodes, cells) = tables(&grid);
    let mut rng = Rng(0x7e12_b01e);
    let distance: Vec<f32> = (0..grid.total())
        .map(|_| {
            if rng.unit() < 0.7 {
                -rng.unit()
            } else {
                rng.unit()
            }
        })
        .collect();
    // One record per cell of the node box: a face has no weight a fifth of the time.
    let faces: Vec<FaceSample> = (0..grid.nodes.iter().product::<u32>())
        .map(|_| {
            let velocity: [f32; 3] = std::array::from_fn(|_| rng.unit() * 2.0 - 1.0);
            let weight: [f32; 3] =
                std::array::from_fn(|_| if rng.unit() < 0.8 { 1.0 } else { 0.0 });
            FaceSample {
                velocity: [velocity[0], velocity[1], velocity[2], 0.0],
                weight: [weight[0], weight[1], weight[2], 0.0],
            }
        })
        .collect();
    let mut harness = Harness::new();
    let len = elements(&nodes);
    let (distance_in, faces_in, tiles) = (
        harness.array(&cells.to_pool(&distance, cpu::canonical_distance(H)), len),
        harness.array(&nodes.to_pool(&faces, FaceSample::default()), len),
        harness.array(&nodes.words, nodes.words.len()),
    );
    let pooled: Vec<f32> = run(
        &mut harness,
        &mut TurbulenceField::new(),
        &[
            ("distance", distance_in.0),
            ("faces", faces_in.0),
            ("tiles", tiles.0),
        ],
        len,
        &grid_params(nodes.pool_tiles),
    );
    let got = cells.from_pool(&pooled);
    let mut busy = 0;
    for (i, &g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::turbulence_field(&grid, &distance, &faces, H, c);
        busy += usize::from(want != 0.0);
        assert!(
            (g - want).abs() <= 1e-4 * want.abs().max(1.0),
            "cell {c:?}: GPU {g} CPU {want}"
        );
    }
    assert!(busy > 100, "{busy} liquid cells are turbulent");
    // Air, and every element the box leaves out, is zero.
    assert_eq!(
        pooled.iter().filter(|&&t| t != 0.0).count(),
        busy,
        "the pool holds turbulence outside the box"
    );
}
