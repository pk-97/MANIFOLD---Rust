use super::FieldSmoothBlend;
use crate::field_build::{FIELD_LEASH_CELLS, FIELD_REDISTANCE_CELLS, FIELD_SMOOTH_STEPS};
use crate::primitives::smooth_lattice::SmoothLattice;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const NODES: [usize; 3] = [23, 19, 27];
const SIZE: [f32; 3] = [2.2, 1.8, 2.6];
const BAND_CELLS: f32 = FIELD_REDISTANCE_CELLS;

fn node_count() -> usize {
    NODES.iter().product()
}

fn position(i: usize) -> [f32; 3] {
    let n = [
        i % NODES[0],
        (i / NODES[0]) % NODES[1],
        i / (NODES[0] * NODES[1]),
    ];
    [0, 1, 2].map(|a| n[a] as f32 * SIZE[a] / (NODES[a] - 1) as f32)
}

/// A sphere distance with a bump pattern on it, and the exact sphere distance.
fn fields() -> (Vec<f32>, Vec<f32>) {
    let centre = SIZE.map(|s| s * 0.5);
    let mut bumpy = Vec::new();
    let mut exact = Vec::new();
    for i in 0..node_count() {
        let p = position(i);
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
    let cell = (0..3)
        .map(|a| SIZE[a] / (NODES[a] - 1) as f32)
        .fold(f32::MAX, f32::min);
    let (leash, band) = (smoothness * FIELD_LEASH_CELLS * cell, BAND_CELLS * cell);
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

fn axis_params(axis: usize) -> ParamValues {
    params(&[
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("passes", 1.0),
        ("axis", axis as f32),
    ])
}

fn blend_params(smoothness: f32) -> ParamValues {
    params(&[
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("size_x", SIZE[0]),
        ("size_y", SIZE[1]),
        ("size_z", SIZE[2]),
        ("smoothness", smoothness),
        ("band_cells", BAND_CELLS),
    ])
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
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
    let n = node_count();
    let (start, reference) = fields();
    let weights = weights();
    let (weights_slot, _) = h.array(&weights, n);
    let (reference_slot, _) = h.array(&reference, n);
    let (start_slot, _) = h.array(&start, n);
    let mut smooth = SmoothLattice::new();
    let mut blend = FieldSmoothBlend::new();
    let mut input = start_slot;
    let mut cpu = start.clone();
    let mut bitten = 0;
    for step in 0..FIELD_SMOOTH_STEPS {
        let mut from = input;
        for axis in 0..3 {
            let (slot, _) = h.array::<f32>(&[], n);
            let (_, errors) = h.run(
                &mut smooth,
                &[("levelset", from)],
                &[("smoothed", slot)],
                &axis_params(axis),
            );
            assert!(errors.is_empty(), "{errors:?}");
            from = slot;
        }
        let (out_slot, out_buf) = h.array::<f32>(&[], n);
        let (_, errors) = h.run(
            &mut blend,
            &[
                ("smoothed", from),
                ("prior", input),
                ("weights", weights_slot),
                ("reference", reference_slot),
            ],
            &[("field", out_slot)],
            &blend_params(smoothness),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plain = cpu_axis(&cpu_axis(&cpu_axis(&cpu, 0), 1), 2);
        let want = cpu_blend(&plain, &cpu, &weights, &reference, smoothness);
        let got = read::<f32>(&out_buf, n);
        for i in 0..n {
            let tolerance = 1e-6 * want[i].abs().max(1.0);
            assert!(
                (got[i] - want[i]).abs() <= tolerance,
                "step {step} node {i}: gpu {} vs cpu {}",
                got[i],
                want[i]
            );
            bitten += usize::from((want[i] - plain[i]).abs() > 1e-4);
        }
        cpu = want;
        input = out_slot;
    }
    // The blend must actually bite: some nodes differ from the plain smoothing.
    assert!(
        bitten > n / 20,
        "{bitten} node-steps differ from plain smoothing"
    );
}

#[test]
fn weight_zero_and_out_of_band_keep_the_prior_bit_for_bit() {
    let mut h = Harness::new();
    let n = node_count();
    let (smoothed, reference) = fields();
    let weights = weights();
    // Within the leash of the reference, so weight 0 leaves the prior untouched.
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let (smoothed_slot, _) = h.array(&smoothed, n);
    let (prior_slot, _) = h.array(&prior, n);
    let (weights_slot, _) = h.array(&weights, n);
    let (reference_slot, _) = h.array(&reference, n);
    let (out_slot, out_buf) = h.array::<f32>(&[], n);
    let mut blend = FieldSmoothBlend::new();
    let (_, errors) = h.run(
        &mut blend,
        &[
            ("smoothed", smoothed_slot),
            ("prior", prior_slot),
            ("weights", weights_slot),
            ("reference", reference_slot),
        ],
        &[("field", out_slot)],
        &blend_params(1.0),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let got = read::<f32>(&out_buf, n);
    let cell = (0..3)
        .map(|a| SIZE[a] / (NODES[a] - 1) as f32)
        .fold(f32::MAX, f32::min);
    let (leash, band) = (FIELD_LEASH_CELLS * cell, BAND_CELLS * cell);
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
}

#[test]
fn zero_smoothness_returns_the_reference_inside_the_band() {
    let mut h = Harness::new();
    let n = node_count();
    let (smoothed, reference) = fields();
    let weights = weights();
    let prior: Vec<f32> = reference
        .iter()
        .enumerate()
        .map(|(i, r)| r + 0.01 * ((i % 5) as f32 - 2.0) / 2.0)
        .collect();
    let (smoothed_slot, _) = h.array(&smoothed, n);
    let (prior_slot, _) = h.array(&prior, n);
    let (weights_slot, _) = h.array(&weights, n);
    let (reference_slot, _) = h.array(&reference, n);
    let (out_slot, out_buf) = h.array::<f32>(&[], n);
    let mut blend = FieldSmoothBlend::new();
    let (_, errors) = h.run(
        &mut blend,
        &[
            ("smoothed", smoothed_slot),
            ("prior", prior_slot),
            ("weights", weights_slot),
            ("reference", reference_slot),
        ],
        &[("field", out_slot)],
        &blend_params(0.0),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let got = read::<f32>(&out_buf, n);
    let cell = (0..3)
        .map(|a| SIZE[a] / (NODES[a] - 1) as f32)
        .fold(f32::MAX, f32::min);
    let band = BAND_CELLS * cell;
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
}

#[test]
fn missing_or_short_inputs_are_named_errors() {
    let mut h = Harness::new();
    let n = node_count();
    let (field, reference) = fields();
    let sentinel = vec![7.5_f32; n];
    let (field_slot, _) = h.array(&field, n);
    let (weights_slot, _) = h.array(&weights(), n);
    let (reference_slot, _) = h.array(&reference, n);
    let (short_slot, _) = h.array(&reference[..n - 1], n - 1);
    let (out_slot, out_buf) = h.array(&sentinel, n);
    let mut blend = FieldSmoothBlend::new();

    let (_, errors) = h.run(
        &mut blend,
        &[
            ("smoothed", field_slot),
            ("prior", field_slot),
            ("reference", reference_slot),
        ],
        &[("field", out_slot)],
        &blend_params(1.0),
    );
    assert!(
        errors.iter().any(|e| e.contains("Field Smooth Blend")),
        "{errors:?}"
    );
    assert_eq!(bits(&read::<f32>(&out_buf, n)), bits(&sentinel));

    let (_, errors) = h.run(
        &mut blend,
        &[
            ("smoothed", field_slot),
            ("prior", field_slot),
            ("weights", weights_slot),
            ("reference", short_slot),
        ],
        &[("field", out_slot)],
        &blend_params(1.0),
    );
    assert!(
        errors.iter().any(|e| e.contains("Field Smooth Blend")),
        "{errors:?}"
    );
    assert_eq!(bits(&read::<f32>(&out_buf, n)), bits(&sentinel));
}
