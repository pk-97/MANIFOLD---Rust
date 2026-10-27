use super::FieldSmoothBlend;
use crate::field_build::{FIELD_BAND_CELLS, FIELD_LEASH_CELLS, FIELD_SMOOTH_STEPS};
use crate::primitives::smooth_lattice::SmoothLattice;
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
/// Nodes in from a face of the box that the three smoothing steps' taps
/// reach: nearer, they read past the box, which the dense reference clamps
/// and the pool does not.
const REACH: usize = FIELD_SMOOTH_STEPS as usize;

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

    /// Elements of `pooled` outside the box that are not `value`: the
    /// tiles' other nodes, the free slot and the canonical element.
    fn outside_not(&self, pooled: &[f32], value: f32) -> usize {
        let mut inside = vec![false; pooled.len()];
        let [nx, ny, nz] = self.table.cells;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    inside[self.table.pool_index([x, y, z])] = true;
                }
            }
        }
        pooled
            .iter()
            .zip(&inside)
            .filter(|&(&v, &in_box)| !in_box && v != value)
            .count()
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

/// One [1 2 1] / 4 pass along `axis`, edge-clamped, summed in the shader's order.
fn cpu_axis(field: &[f32], axis: usize) -> Vec<f32> {
    let stride = [1, NODES[0], NODES[0] * NODES[1]];
    (0..field.len())
        .map(|i| {
            let at = (i / stride[axis]) % NODES[axis];
            let mut sum = 0.0_f32;
            for (d, w) in [(-1_i32, 0.25_f32), (0, 0.5), (1, 0.25)] {
                let q = (at as i32 + d).clamp(0, NODES[axis] as i32 - 1) as usize;
                sum += w * field[i - at * stride[axis] + q * stride[axis]];
            }
            sum
        })
        .collect()
}

fn cpu_blend(
    smoothed: &[f32],
    prior: &[f32],
    weights: &[f32],
    reference: &[f32],
    smoothness: f32,
) -> Vec<f32> {
    let (leash, band) = (smoothness * FIELD_LEASH_CELLS * CELL, BAND_CELLS * CELL);
    (0..smoothed.len())
        .map(|i| {
            if reference[i].abs() >= band {
                prior[i]
            } else {
                (prior[i] + weights[i] * (smoothed[i] - prior[i]))
                    .clamp(reference[i] - leash, reference[i] + leash)
            }
        })
        .collect()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn axis_params(placed: &Placed, axis: usize, passes: usize) -> ParamValues {
    placed.params(&[("passes", passes as f32), ("axis", axis as f32)])
}

fn blend_params(placed: &Placed, smoothness: f32) -> ParamValues {
    placed.params(&[("smoothness", smoothness), ("band_cells", BAND_CELLS)])
}

/// The blend of the four dense fields placed in `placed`, as a pooled array.
fn blend(
    h: &mut Harness,
    placed: &Placed,
    [smoothed, prior, weights, reference]: [&[f32]; 4],
    smoothness: f32,
) -> Vec<f32> {
    let len = placed.len();
    // Every free-slot element holds water-like junk the blend must not copy.
    let (tiles, _) = h.array(&placed.words, placed.words.len());
    let (smoothed, _) = h.array(&placed.pooled(smoothed, FAR, 9.0), len);
    let (prior, _) = h.array(&placed.pooled(prior, FAR, 9.0), len);
    let (weights, _) = h.array(&placed.pooled(weights, 0.0, 9.0), len);
    let (reference, _) = h.array(&placed.pooled(reference, FAR, 9.0), len);
    let (out, out_buf) = h.array::<f32>(&[], len);
    let (_, errors) = h.run(
        &mut FieldSmoothBlend::new(),
        &[
            ("smoothed", smoothed),
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

#[test]
fn three_steps_match_the_cpu_reference() {
    // Smoothness 1 is the tuned surface; 1.7 checks the leash scales with it.
    for smoothness in [1.0, 1.7] {
        three_steps_match(smoothness);
    }
}

fn three_steps_match(smoothness: f32) {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let len = placed.len();
    let (start, reference) = fields();
    let weights = weights();
    let (tiles, _) = h.array(&placed.words, placed.words.len());
    let (weights_slot, _) = h.array(&placed.pooled(&weights, 0.0, 9.0), len);
    let (reference_slot, _) = h.array(&placed.pooled(&reference, FAR, 9.0), len);
    let (start_slot, _) = h.array(&placed.pooled(&start, FAR, 9.0), len);
    let mut smooth = SmoothLattice::new();
    let mut blend = FieldSmoothBlend::new();
    let mut input = start_slot;
    let mut cpu = start.clone();
    let (mut bitten, mut checked) = (0, 0);
    for step in 0..FIELD_SMOOTH_STEPS {
        let mut from = input;
        for axis in 0..3 {
            let (slot, _) = h.array::<f32>(&[], len);
            let (_, errors) = h.run(
                &mut smooth,
                &[("levelset", from), ("tiles", tiles)],
                &[("smoothed", slot)],
                &axis_params(&placed, axis, 1),
            );
            assert!(errors.is_empty(), "{errors:?}");
            from = slot;
        }
        let (out_slot, out_buf) = h.array::<f32>(&[], len);
        let (_, errors) = h.run(
            &mut blend,
            &[
                ("smoothed", from),
                ("prior", input),
                ("weights", weights_slot),
                ("reference", reference_slot),
                ("tiles", tiles),
            ],
            &[("field", out_slot)],
            &blend_params(&placed, smoothness),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plain = cpu_axis(&cpu_axis(&cpu_axis(&cpu, 0), 1), 2);
        let want = cpu_blend(&plain, &cpu, &weights, &reference, smoothness);
        let pooled = read::<f32>(&out_buf, len);
        let got = placed.dense(&pooled);
        for i in (0..node_count()).filter(|&i| interior(i)) {
            let tolerance = 1e-6 * want[i].abs().max(1.0);
            assert!(
                (got[i] - want[i]).abs() <= tolerance,
                "step {step} node {:?}: gpu {} vs cpu {}",
                node_at(i),
                got[i],
                want[i]
            );
            bitten += usize::from((want[i] - plain[i]).abs() > 1e-4);
            checked += 1;
        }
        // Nothing the box leaves out holds water: its weight is 0 and its
        // prior the canonical level set.
        assert_eq!(placed.outside_not(&pooled, FAR), 0, "step {step}");
        cpu = want;
        input = out_slot;
    }
    // The blend must actually bite: some nodes differ from the plain smoothing.
    assert!(
        bitten > checked / 20,
        "{bitten} of {checked} node-steps differ from plain smoothing"
    );
}

/// `passes` rounds of [1 2 1] / 4 along `axis` as the binomial row C(2p, k) /
/// 4^p summed in the shader's order, a tap past the box the canonical level
/// set.
fn cpu_smooth(field: &[f32], nodes: [usize; 3], axis: usize, passes: usize) -> Vec<f32> {
    let n = 2 * passes;
    let mut choose = vec![1_u32; n + 1];
    for k in 1..n {
        choose[k] = choose[k - 1] * (n - k + 1) as u32 / k as u32;
    }
    let row: Vec<f32> = choose
        .iter()
        .map(|&c| c as f32 / 4_f32.powi(passes as i32))
        .collect();
    let stride = [1, nodes[0], nodes[0] * nodes[1]];
    (0..field.len())
        .map(|i| {
            let at = (i / stride[axis]) % nodes[axis];
            let mut sum = 0.0_f32;
            for d in -(passes as i32)..=passes as i32 {
                let q = at as i32 + d;
                let tap = if q < 0 || q >= nodes[axis] as i32 {
                    FAR
                } else {
                    field[i - at * stride[axis] + q as usize * stride[axis]]
                };
                sum += row[(d + passes as i32) as usize] * tap;
            }
            sum
        })
        .collect()
}

/// A box that fills its tiles to their faces, across tiles -1 and 0 on x: a
/// tap past it is in a tile the table does not hold and reads the canonical
/// level set, not the box's edge. The free slot comes out canonical too.
#[test]
fn smoothing_reads_the_canonical_level_set_past_the_tiles_it_holds() {
    let nodes = [16_usize, 8, 8];
    let placed = Placed::new([-8, 0, 0], nodes);
    let field: Vec<f32> = (0..nodes.iter().product::<usize>())
        .map(|i| ((i * 37) % 101) as f32 * 0.01 - 0.5)
        .collect();
    let mut h = Harness::new();
    let len = placed.len();
    let (tiles, _) = h.array(&placed.words, placed.words.len());
    let (input, _) = h.array(&placed.pooled(&field, FAR, -1.0), len);
    for passes in 0..=3 {
        for axis in 0..3 {
            let (out, out_buf) = h.array::<f32>(&[], len);
            let (_, errors) = h.run(
                &mut SmoothLattice::new(),
                &[("levelset", input), ("tiles", tiles)],
                &[("smoothed", out)],
                &axis_params(&placed, axis, passes),
            );
            assert!(errors.is_empty(), "{errors:?}");
            let pooled = read::<f32>(&out_buf, len);
            let want = cpu_smooth(&field, nodes, axis, passes);
            for (i, (g, w)) in placed.dense(&pooled).iter().zip(&want).enumerate() {
                assert!(
                    (g - w).abs() <= 1e-6,
                    "{passes} passes axis {axis} node {i}: gpu {g} vs cpu {w}"
                );
            }
            assert_eq!(
                placed.outside_not(&pooled, FAR),
                0,
                "{passes} passes axis {axis}"
            );
        }
    }
}

#[test]
fn weight_zero_and_out_of_band_keep_the_prior_bit_for_bit() {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let n = node_count();
    let (smoothed, reference) = fields();
    let weights = weights();
    // Within the leash of the reference, so weight 0 leaves the prior untouched.
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let pooled = blend(
        &mut h,
        &placed,
        [&smoothed, &prior, &weights, &reference],
        1.0,
    );
    let got = placed.dense(&pooled);
    let (leash, band) = (FIELD_LEASH_CELLS * CELL, BAND_CELLS * CELL);
    let (mut held, mut in_band) = (0, 0);
    for i in 0..n {
        if reference[i].abs() >= band || weights[i] == 0.0 {
            assert_eq!(got[i].to_bits(), prior[i].to_bits(), "node {i}");
            held += 1;
        }
        if reference[i].abs() < band {
            assert!(
                (got[i] - reference[i]).abs() <= leash * (1.0 + 1e-6) + 1e-6,
                "node {i}: {} vs reference {}",
                got[i],
                reference[i]
            );
            in_band += 1;
        }
    }
    assert!(held > 0 && in_band > 0, "held {held}, in band {in_band}");
    assert_ne!(bits(&got), bits(&prior));
    // The free slot (full of junk on every input) and the rest of the pool
    // come out canonical.
    assert_eq!(placed.outside_not(&pooled, FAR), 0);
}

#[test]
fn zero_smoothness_returns_the_reference_inside_the_band() {
    let mut h = Harness::new();
    let placed = Placed::new(ORIGIN, NODES);
    let n = node_count();
    let (smoothed, reference) = fields();
    let weights = weights();
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let pooled = blend(
        &mut h,
        &placed,
        [&smoothed, &prior, &weights, &reference],
        0.0,
    );
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
    assert_eq!(placed.outside_not(&pooled, FAR), 0);
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
                ("smoothed", field_slot),
                ("prior", field_slot),
                ("reference", reference_slot),
                ("tiles", tiles),
            ],
        ),
        (
            "a reference one short of the pool",
            vec![
                ("smoothed", field_slot),
                ("prior", field_slot),
                ("weights", weights_slot),
                ("reference", short_slot),
                ("tiles", tiles),
            ],
        ),
        (
            "no tiles",
            vec![
                ("smoothed", field_slot),
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
