use super::FieldSmoothBlend;
use crate::field_build::{FIELD_BAND_CELLS, FIELD_LEASH_CELLS, FIELD_SMOOTH_STEPS};
use crate::testkit::world_box::{BoxTable, from_pool_under, table_words_of};
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const NODES: [usize; 3] = [23, 19, 27];
const CELL: f32 = 0.1;
/// The world node of the box's first node: x from -6 puts tiles -1 and 0 in
/// the box, y and z start inside their tiles.
const ORIGIN: [i32; 3] = [-6, 3, -5];
const BAND_CELLS: f32 = FIELD_BAND_CELLS;
/// The canonical level set, `FIELD_FAR_CELLS` (3) cells.
const FAR: f32 = 3.0 * CELL;
/// Nodes in from a face of the box that the three smoothing steps' samples
/// reach (two nodes a step at one display node per simulation cell): nearer,
/// they read the canonical level set past the box.
const REACH: usize = 2 * FIELD_SMOOTH_STEPS as usize;

fn node_count() -> usize {
    NODES.iter().product()
}

fn node_at(i: usize) -> [usize; 3] {
    [
        i % NODES[0],
        (i / NODES[0]) % NODES[1],
        i / (NODES[0] * NODES[1]),
    ]
}

/// Whether node `i` of the box is `REACH` nodes in from every face.
fn interior(i: usize) -> bool {
    let at = node_at(i);
    (0..3).all(|a| at[a] >= REACH && at[a] + REACH < NODES[a])
}

/// A box of nodes placed in world tiles, with one free slot after the tiles
/// it touches: the arrays on it are pooled arrays of that table.
struct Placed {
    table: BoxTable,
    words: Vec<u32>,
    pool_tiles: u32,
}

impl Placed {
    fn new(origin: [i32; 3], nodes: [usize; 3]) -> Self {
        let table = BoxTable::new(origin, nodes.map(|n| n as u32));
        let pool_tiles = table.pool_tiles + 1;
        let words = table_words_of(pool_tiles, &table.held());
        Self {
            table,
            words,
            pool_tiles,
        }
    }

    fn len(&self) -> usize {
        pool_len(self.pool_tiles) as usize
    }

    /// `dense` (x fastest) as a pooled array: `canon` in every node of the
    /// tiles outside the box and in the canonical element, `junk` in the
    /// free slot.
    fn pooled(&self, dense: &[f32], canon: f32, junk: f32) -> Vec<f32> {
        let mut pool = self.table.to_pool(dense, canon);
        pool.pop();
        pool.extend(vec![junk; TILE_CELLS as usize]);
        pool.push(canon);
        pool
    }

    /// The box's nodes (x fastest) out of a pooled array.
    fn dense(&self, pooled: &[f32]) -> Vec<f32> {
        from_pool_under(
            pooled,
            &self.words,
            self.pool_tiles,
            self.table.origin,
            self.table.cells,
        )
    }

    /// Elements of the free slot and the canonical element of `pooled`
    /// that are not `value`.
    fn free_not(&self, pooled: &[f32], value: f32) -> usize {
        let free = self.table.pool_tiles as usize * TILE_CELLS as usize;
        pooled[free..].iter().filter(|&&v| v != value).count()
    }

    fn params(&self, extra: &[(&'static str, f32)]) -> ParamValues {
        let mut all = vec![("cell_size", CELL), ("pool_tiles", self.pool_tiles as f32)];
        all.extend_from_slice(extra);
        params(&all)
    }
}

/// A sphere distance with a bump pattern on it, and the exact sphere distance.
fn fields() -> (Vec<f32>, Vec<f32>) {
    let centre = NODES.map(|n| (n - 1) as f32 * CELL * 0.5);
    let mut bumpy = Vec::new();
    let mut exact = Vec::new();
    for i in 0..node_count() {
        let p = node_at(i).map(|n| n as f32 * CELL);
        let r = (0..3)
            .map(|a| (p[a] - centre[a]).powi(2))
            .sum::<f32>()
            .sqrt()
            - 0.63;
        exact.push(r);
        bumpy.push(r + 0.06 * (9.0 * p[0]).sin() * (7.0 * p[1]).cos() * (8.0 * p[2]).sin());
    }
    (bumpy, exact)
}

/// Weights mixing 0, 1 and fractions.
fn weights() -> Vec<f32> {
    (0..node_count())
        .map(|i| match i % 4 {
            0 => 0.0,
            1 => 1.0,
            _ => (i % 7) as f32 / 7.0,
        })
        .collect()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn blend_params(placed: &Placed, smoothness: f32) -> ParamValues {
    placed.params(&[
        ("smoothness", smoothness),
        ("band_cells", BAND_CELLS),
        ("sim_cell_size", CELL),
    ])
}

/// One step over the three dense fields placed in `placed`, as a pooled
/// array.
fn blend(
    h: &mut Harness,
    placed: &Placed,
    [prior, weights, reference]: [&[f32]; 3],
    smoothness: f32,
) -> Vec<f32> {
    let len = placed.len();
    // Every free-slot element holds water-like junk the step must not copy.
    let (tiles, _) = h.array(&placed.words, placed.words.len());
    let (prior, _) = h.array(&placed.pooled(prior, FAR, 9.0), len);
    let (weights, _) = h.array(&placed.pooled(weights, 0.0, 9.0), len);
    let (reference, _) = h.array(&placed.pooled(reference, FAR, 9.0), len);
    let (out, out_buf) = h.array::<f32>(&[], len);
    let (_, errors) = h.run(
        &mut FieldSmoothBlend::new(),
        &[
            ("prior", prior),
            ("weights", weights),
            ("reference", reference),
            ("tiles", tiles),
        ],
        &[("field", out)],
        &blend_params(placed, smoothness),
    );
    assert!(errors.is_empty(), "{errors:?}");
    read::<f32>(&out_buf, len)
}

/// FIELD_SMOOTH_STEPS steps from `prior`, each held to `reference`, as the
/// box's dense nodes.
fn steps(
    placed: &Placed,
    prior: &[f32],
    weights: &[f32],
    reference: &[f32],
    smoothness: f32,
) -> Vec<f32> {
    let mut h = Harness::new();
    let mut field = prior.to_vec();
    for _ in 0..FIELD_SMOOTH_STEPS {
        let pooled = blend(&mut h, placed, [&field, weights, reference], smoothness);
        assert_eq!(placed.free_not(&pooled, FAR), 0);
        field = placed.dense(&pooled);
    }
    field
}

/// A tilted plane's distance, off every axis: the ring round each node lies
/// in the plane through it, so every sample reads the node's own value and
/// the field stays put.
#[test]
fn a_straight_distance_field_stays_put() {
    let placed = Placed::new(ORIGIN, NODES);
    let n = [1.0_f32, 2.0, 0.5];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    let plane: Vec<f32> = (0..node_count())
        .map(|i| {
            let p = node_at(i).map(|v| v as f32 * CELL);
            (n[0] * p[0] + n[1] * p[1] + n[2] * p[2]) / len - 1.4
        })
        .collect();
    let got = steps(&placed, &plane, &weights(), &plane, 2.0);
    let band = BAND_CELLS * CELL;
    let mut checked = 0;
    for i in (0..node_count()).filter(|&i| interior(i) && plane[i].abs() < band) {
        assert!(
            (got[i] - plane[i]).abs() <= 2.0e-5,
            "node {:?}: {} vs {}",
            node_at(i),
            got[i],
            plane[i]
        );
        checked += 1;
    }
    assert!(checked > 500, "{checked}");
}

/// Bumps that move a sphere's surface in and out (each the same along its
/// radius): three steps take out at least a third of them, and no node ends
/// further off the sphere than the furthest started. A sphere shrinks as it
/// smooths, as surface tension shrinks a drop, so both measure each node
/// against the mean over the nodes near the surface.
#[test]
fn bumps_on_a_sphere_flatten() {
    let placed = Placed::new(ORIGIN, NODES);
    let centre = NODES.map(|n| (n - 1) as f32 * CELL * 0.5);
    let (mut bumpy, mut exact) = (Vec::new(), Vec::new());
    for i in 0..node_count() {
        let d: [f32; 3] = std::array::from_fn(|a| node_at(i)[a] as f32 * CELL - centre[a]);
        let r = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
        let u = d.map(|v| v / r);
        exact.push(r - 0.63);
        bumpy.push(
            r - 0.63 - 0.4 * CELL * (6.0 * u[0]).sin() * (5.0 * u[1]).sin() * (4.0 * u[2]).cos(),
        );
    }
    let got = steps(&placed, &bumpy, &vec![1.0; node_count()], &bumpy, 2.0);
    let near: Vec<usize> = (0..node_count())
        .filter(|&i| interior(i) && exact[i].abs() < 1.5 * CELL)
        .collect();
    let off = |f: &[f32]| {
        let mean = near.iter().map(|&i| f[i] - exact[i]).sum::<f32>() / near.len() as f32;
        near.iter()
            .map(|&i| f[i] - exact[i] - mean)
            .collect::<Vec<f32>>()
    };
    let rms = |o: &[f32]| (o.iter().map(|v| v * v).sum::<f32>() / o.len() as f32).sqrt();
    let worst = |o: &[f32]| o.iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
    let (before, after) = (off(&bumpy), off(&got));
    assert!(near.len() > 100, "{}", near.len());
    assert!(
        rms(&after) < 0.67 * rms(&before),
        "{} vs {}",
        rms(&after),
        rms(&before)
    );
    assert!(
        worst(&after) <= worst(&before),
        "{} vs {}",
        worst(&after),
        worst(&before)
    );
}

/// A wavy film 0.6 cells thick, its middle wandering across a row of nodes:
/// where it sits on the row the gradient there is near nothing, and only the
/// bend tells the ring to lie along the film. Every column still holds water
/// after three steps at full weight and the widest leash.
#[test]
fn a_thin_film_stays_whole() {
    let placed = Placed::new(ORIGIN, NODES);
    let row = 9.0 * CELL;
    let film: Vec<f32> = (0..node_count())
        .map(|i| {
            let p = node_at(i).map(|v| v as f32 * CELL);
            let middle = row + 0.1 * CELL * (9.0 * p[0]).sin() * (8.0 * p[2]).cos();
            (p[1] - middle).abs() - 0.3 * CELL
        })
        .collect();
    let got = steps(&placed, &film, &vec![1.0; node_count()], &film, 2.0);
    let mut columns = 0;
    for z in REACH..NODES[2] - REACH {
        for x in REACH..NODES[0] - REACH {
            let column = (0..NODES[1]).map(|y| got[x + NODES[0] * (y + NODES[1] * z)]);
            let lowest = column.fold(f32::INFINITY, f32::min);
            assert!(lowest < 0.0, "column ({x}, {z}) opened: lowest {lowest}");
            columns += 1;
        }
    }
    assert!(columns > 100);
}

#[test]
fn out_of_band_keeps_the_prior_bit_for_bit_and_the_leash_holds() {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let n = node_count();
    let (_, reference) = fields();
    let weights = weights();
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let pooled = blend(&mut h, &placed, [&prior, &weights, &reference], 1.0);
    let got = placed.dense(&pooled);
    let (leash, band) = (FIELD_LEASH_CELLS * CELL, BAND_CELLS * CELL);
    let (mut held, mut in_band) = (0, 0);
    for i in 0..n {
        if reference[i].abs() >= band {
            assert_eq!(got[i].to_bits(), prior[i].to_bits(), "node {i}");
            held += 1;
        } else {
            // Thin water (weight 0) moves within one leash, bulk within two.
            let reach = (1.0 + weights[i]) * leash;
            assert!(
                (got[i] - reference[i]).abs() <= reach * (1.0 + 1e-6) + 1e-6,
                "node {i}: {} vs reference {}",
                got[i],
                reference[i]
            );
            in_band += 1;
        }
    }
    assert!(held > 0 && in_band > 0, "held {held}, in band {in_band}");
    assert_ne!(bits(&got), bits(&prior));
    // The free slot (full of junk on every input) and the canonical element
    // come out far.
    assert_eq!(placed.free_not(&pooled, FAR), 0);
}

#[test]
fn zero_smoothness_returns_the_reference_inside_the_band() {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let n = node_count();
    let (_, reference) = fields();
    let weights = weights();
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let pooled = blend(&mut h, &placed, [&prior, &weights, &reference], 0.0);
    let got = placed.dense(&pooled);
    let band = BAND_CELLS * CELL;
    let mut in_band = 0;
    for i in 0..n {
        if reference[i].abs() < band {
            assert!(
                (got[i] - reference[i]).abs() <= 1e-6 * reference[i].abs().max(1.0),
                "node {i}: {} vs reference {}",
                got[i],
                reference[i]
            );
            in_band += 1;
        }
    }
    assert!(in_band > 0, "no node inside the band");
    assert_eq!(placed.free_not(&pooled, FAR), 0);
}

#[test]
fn missing_or_short_inputs_are_named_errors() {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let len = placed.len();
    let (field, reference) = fields();
    let sentinel = vec![7.5_f32; len];
    let (tiles, _) = h.array(&placed.words, placed.words.len());
    let (field_slot, _) = h.array(&placed.pooled(&field, FAR, 9.0), len);
    let (weights_slot, _) = h.array(&placed.pooled(&weights(), 0.0, 9.0), len);
    let pooled_reference = placed.pooled(&reference, FAR, 9.0);
    let (reference_slot, _) = h.array(&pooled_reference, len);
    let (short_slot, _) = h.array(&pooled_reference[..len - 1], len - 1);
    let (out_slot, out_buf) = h.array(&sentinel, len);
    let mut blend = FieldSmoothBlend::new();

    let cases: [(&str, Vec<(&'static str, _)>); 3] = [
        (
            "no weights",
            vec![
                ("prior", field_slot),
                ("reference", reference_slot),
                ("tiles", tiles),
            ],
        ),
        (
            "a reference one short of the pool",
            vec![
                ("prior", field_slot),
                ("weights", weights_slot),
                ("reference", short_slot),
                ("tiles", tiles),
            ],
        ),
        (
            "no tiles",
            vec![
                ("prior", field_slot),
                ("weights", weights_slot),
                ("reference", reference_slot),
            ],
        ),
    ];
    for (what, inputs) in cases {
        let (_, errors) = h.run(
            &mut blend,
            &inputs,
            &[("field", out_slot)],
            &blend_params(&placed, 1.0),
        );
        assert!(
            errors.iter().any(|e| e.contains("Field Smooth Blend")),
            "{what}: {errors:?}"
        );
        assert_eq!(bits(&read::<f32>(&out_buf, len)), bits(&sentinel), "{what}");
    }
}
