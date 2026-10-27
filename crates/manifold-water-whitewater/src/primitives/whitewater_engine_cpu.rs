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
    check::<manifold_water_liquid::primitives::upwind_distance::UpwindDistance>();
    check::<super::advect_whitewater::AdvectWhitewater>();
    check::<super::keep_whitewater::KeepWhitewater>();
    validate(&manifold_water_liquid::primitives::whitewater_distance::whitewater_distance_shader());
}
