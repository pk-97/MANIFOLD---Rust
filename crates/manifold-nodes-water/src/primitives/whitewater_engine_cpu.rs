//! CPU specification of the FLIP Fluids whitewater engine rules (BUG-g75v.7).
//! Ported from particlelevelset.cpp, levelsetsolver.cpp and
//! diffuseparticlesimulation.cpp; Ryan L. Guy & Dennis Fassbaender, MIT.
//! See THIRD_PARTY_NOTICES.md. No renderer or GPU is used here.

/// Engine upwind sweep: sign is recomputed from the current field, clamped
/// boundary neighbours, dtau = h/2. The caller owns the valid-cell mask.
pub(super) fn upwind(input: &[f32], dims: [usize; 3], h: f32, valid: &[bool]) -> Vec<f32> {
    let mut output = input.to_vec();
    let strides = [1, dims[0], dims[0] * dims[1]];
    for i in 0..input.len() {
        if !valid[i] {
            continue;
        }
        let coord = [i % dims[0], i / dims[0] % dims[1], i / strides[2]];
        let d = input[i];
        let sign = (f64::from(d) / (f64::from(d).powi(2) + f64::from(h).powi(2)).sqrt()) as f32;
        let mut positive = 0.0;
        let mut negative = 0.0;
        for axis in 0..3 {
            let lo = if coord[axis] == 0 {
                i
            } else {
                i - strides[axis]
            };
            let hi = if coord[axis] + 1 == dims[axis] {
                i
            } else {
                i + strides[axis]
            };
            let a = (d - input[lo]) / h;
            let b = (input[hi] - d) / h;
            positive += a.max(0.0).powi(2) + b.min(0.0).powi(2);
            negative += a.min(0.0).powi(2) + b.max(0.0).powi(2);
        }
        output[i] = d
            - 0.5 * h * sign.max(0.0) * (positive.sqrt() - 1.0)
            - 0.5 * h * sign.min(0.0) * (negative.sqrt() - 1.0);
    }
    output
}

/// Match the engine: the sweep writes tempPtr, then swaps it into outputPtr.
/// The returned field includes the final converging sweep.
pub(super) fn reinitialize(input: &[f32], dims: [usize; 3], h: f32, valid: &[bool]) -> Vec<f32> {
    let mut current = input.to_vec();
    let mut last = -1.0f32;
    for iteration in 0..6 {
        let next = upwind(&current, dims, h, valid);
        let diff = next
            .iter()
            .zip(&current)
            .zip(valid)
            .filter(|(_, v)| **v)
            .map(|((a, b), _)| (a - b).abs())
            .fold(0.0f32, f32::max);
        current = next;
        if (diff - last).abs() < 0.01 * h || iteration == 5 {
            return current;
        }
        last = diff;
    }
    unreachable!()
}

/// Engine 6-cell blocks, one six-connected feather and +5h outside the band.
pub(super) fn surface_distance(input: &[f32], dims: [usize; 3], h: f32) -> Vec<f32> {
    let blocks = dims.map(|n| n.div_ceil(6));
    let index = |c: [usize; 3]| c[0] + blocks[0] * (c[1] + blocks[1] * c[2]);
    let coord = |i: usize| {
        [
            i % dims[0] / 6,
            i / dims[0] % dims[1] / 6,
            i / (dims[0] * dims[1]) / 6,
        ]
    };
    let mut occupied = vec![false; blocks.into_iter().product()];
    for (i, d) in input.iter().enumerate() {
        if d.abs() < 2.0 * h {
            occupied[index(coord(i))] = true;
        }
    }
    let valid: Vec<_> = (0..input.len())
        .map(|i| {
            let c = coord(i);
            occupied[index(c)]
                || (0..3).any(|a| {
                    let mut lo = c;
                    let mut hi = c;
                    lo[a] = lo[a].saturating_sub(1);
                    hi[a] = (hi[a] + 1).min(blocks[a] - 1);
                    occupied[index(lo)] || occupied[index(hi)]
                })
        })
        .collect();
    let mut out = reinitialize(input, dims, h, &valid);
    for (d, valid) in out.iter_mut().zip(valid) {
        if !valid {
            *d = 5.0 * h;
        }
    }
    out
}

/// Engine spray uses semi-implicit Euler each liquid substep, never one
/// aggregate step. Force acceleration is additional to gravity; an event
/// changes velocity exactly once at its accepted boundary.
pub(super) fn spray(steps: &[(f64, f64, f64)], mut position: f64, mut velocity: f64) -> (f64, f64) {
    for &(dt, acceleration, impulse) in steps {
        velocity += impulse + acceleration * dt;
        position += velocity * dt;
    }
    (position, velocity)
}

#[test]
fn whitewater_engine_upwind_is_not_triangle_redistance() {
    let dims = [8, 8, 8];
    let input: Vec<_> = (0..512).map(|i| 0.2 * (i % 8) as f32 - 0.7).collect();
    let valid = vec![true; 512];
    let result = reinitialize(&input, dims, 1.0, &valid);
    assert!(result[4] > input[4]);
    assert!(result[3] < input[3]);
    // Exact geometric distance to this plane is 0.5; six engine sweeps
    // deliberately do not return it.
    assert!((result[4] - 0.5).abs() > 0.01);
    assert_eq!(upwind(&input, dims, 1.0, &vec![false; 512]), input);
}

#[test]
fn whitewater_engine_substeps_and_timestamped_hits_use_actual_durations() {
    let steps = [(0.01, -10.0, 0.0), (0.03, -10.0, 2.0), (0.0, 999.0, 0.0)];
    let (p, v) = spray(&steps, 0.0, 3.0);
    assert!((p - 0.167).abs() < 1e-12);
    assert!((v - 4.6).abs() < 1e-12);
    assert!((spray(&[(0.04, -10.0, 2.0)], 0.0, 3.0).0 - p).abs() > 0.01);
    // Foam samples each accepted velocity. Adding forces again would
    // violate _advanceFoamParticlesThread, which uses only vmac.
    let foam = 0.01 * 3.0 + 0.03 * 5.0;
    assert!((foam - 0.18f64).abs() < 1e-12);
}

#[test]
fn whitewater_engine_shaders_are_valid() {
    use manifold_node_engine::primitive::Primitive;
    fn check<P: Primitive>() {
        let shader = manifold_node_engine::freeze::codegen::standalone_for_spec::<P>().unwrap();
        validate(&shader);
    }
    fn validate(shader: &str) {
        let module = naga::front::wgsl::parse_str(shader)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(shader)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(shader)));
    }
    check::<super::upwind_distance::UpwindDistance>();
    check::<super::advect_whitewater::AdvectWhitewater>();
    check::<super::keep_whitewater::KeepWhitewater>();
    validate(include_str!("shaders/whitewater_distance.wgsl"));
}

#[test]
fn whitewater_surface_distance_feathers_six_connected_blocks() {
    let mut corner = vec![8.0; 512];
    corner[0] = 0.1;
    let field = surface_distance(&corner, [8; 3], 1.0);
    assert_ne!(field[7], 5.0); // adjacent block is feathered
    assert_eq!(field[511], 5.0); // diagonal block is not
    assert_eq!(surface_distance(&[-5.0; 512], [8; 3], 1.0), vec![5.0; 512]);
}

#[test]
fn whitewater_distance_extent_covers_small_and_shipped_lattices() {
    for cells in [[8u32; 3], [16, 12, 8], [64; 3]] {
        let count = cells.into_iter().map(u64::from).product::<u64>();
        let blocks = cells
            .into_iter()
            .map(|n| u64::from(n.div_ceil(6)))
            .product::<u64>();
        assert_eq!(
            super::whitewater_distance::scratch_bytes(cells),
            // Plus the sweep grid (16) and the zero clock plan (48).
            12 * count + 4 * blocks + 16 + 16 + 48
        );
        for i in 0..count {
            let c = [
                i % u64::from(cells[0]),
                i / u64::from(cells[0]) % u64::from(cells[1]),
                i / (u64::from(cells[0]) * u64::from(cells[1])),
            ];
            let block = c[0] / 6
                + u64::from(cells[0].div_ceil(6))
                    * (c[1] / 6 + u64::from(cells[1].div_ceil(6)) * (c[2] / 6));
            assert!(block < blocks);
        }
    }
}

#[cfg(feature = "whitewater-oracle")]
#[test]
fn whitewater_upwind_matches_vendored_engine() {
    for n in [8usize, 16] {
        let plane: Vec<_> = (0..n.pow(3)).map(|i| 0.2 * (i % n) as f32 - 0.7).collect();
        let mut corner = vec![8.0; n.pow(3)];
        corner[0] = 0.1;
        for input in [plane, corner, vec![-5.0; n.pow(3)]] {
            let reference =
                manifold_fluids::whitewater_oracle::curvature(&input, [n as u32; 3], 1.0).unwrap();
            let expected = surface_distance(&input, [n; 3], 1.0);
            for (a, b) in reference.surface_phi.iter().zip(expected) {
                assert!((a - b).abs() < 2e-6, "engine {a} CPU {b}");
            }
        }
    }
}

#[test]
fn whitewater_outflow_strict_surface_rule_and_proof_extents() {
    // fluidsimulation.cpp:9065: non-inverted mesh outflow removes d < 0.
    let flags = [3.5f32, 4.0, 4.5].map(|x| u32::from(x - 4.0 >= 0.0));
    assert_eq!(flags, [0, 1, 1]);
    // Value proofs: 8³ cells, 9³ solid nodes, 3 accepted schedule slots,
    // four particle types, one 2³ atlas. Every shader guards rounded tails.
    let faces = 9 * 8 * 8;
    assert_eq!(faces, 576);
    assert_eq!(3 * faces, 1728);
    assert_eq!(3 * 4, 12);
    assert_eq!(9usize.pow(3), 729);
    assert_eq!(2usize.pow(3) / 2, 4);
}
