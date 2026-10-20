//! CPU references for the industry force set: wind and drag, falloff curves,
//! attractors.

use super::*;

fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() <= tolerance * (1.0 + b.abs()))
}

fn wind(velocity: [f32; 3], rate: f32) -> FieldValue {
    FieldValue::uniform(velocity).unwrap().air(rate).unwrap()
}

/// A body in wind relaxes to the wind's velocity at the rate drag rate ×
/// Air Drag: v(t) = u + (v0 − u) e^(−k t), at any step size.
#[test]
fn drag_converges_a_velocity_to_the_wind_at_its_rate() {
    let u = [3.0, 0.0, -1.0];
    let field = wind(u, 2.0).with_air_drag(1.5).unwrap();
    let sample = field.sample_force([0.4, 1.0, 2.0]);
    assert_eq!(sample.push, [0.0; 3]);
    assert!((sample.drag - 3.0).abs() < 1e-6);
    let k = 3.0f32;
    let v0 = [0.0, 2.0, 0.0];
    for dt in [1.0 / 30.0, 1.0 / 120.0, 0.25] {
        let mut v = v0;
        let mut t = 0.0f32;
        while t < 2.0 {
            v = sample.step_velocity(v, dt);
            t += dt;
            let expected: [f32; 3] = std::array::from_fn(|a| u[a] + (v0[a] - u[a]) * (-k * t).exp());
            assert!(close(v, expected, 1e-4), "dt {dt} t {t}: {v:?} vs {expected:?}");
        }
    }
    // The drag-only delta the rigid path applies is the same step.
    let delta = sample.drag_delta_velocity(v0, 0.1);
    let stepped = sample.step_velocity(v0, 0.1);
    assert!(close(std::array::from_fn(|a| v0[a] + delta[a]), stepped, 1e-6));
}

/// With the consumer's Air Drag at 0, or the wind's drag rate at 0, wind
/// changes nothing: the push alone moves the body.
#[test]
fn wind_with_zero_coefficient_changes_nothing() {
    let push = FieldValue::uniform([0.0, -9.8, 0.0]).unwrap();
    let windy = push.sum(&wind([5.0, 0.0, 0.0], 4.0)).unwrap();
    let v = [1.0, 2.0, 3.0];
    let dt = 1.0 / 60.0;
    let still = push.sample_force([0.0; 3]).step_velocity(v, dt);
    for field in [windy.with_air_drag(0.0).unwrap(), push.sum(&wind([5.0, 0.0, 0.0], 0.0)).unwrap()] {
        let sample = field.sample_force([0.0; 3]);
        assert_eq!(sample.drag, 0.0);
        assert_eq!(sample.drag_wind, [0.0; 3]);
        assert_eq!(sample.step_velocity(v, dt), still);
    }
    // A field with no air ignores Air Drag entirely.
    assert_eq!(push.with_air_drag(3.0).unwrap(), push);
}

/// Two winds add by drag rate: the air moves at their rate-weighted mean and
/// drags at their summed rate. A negative scale reverses the wind and keeps
/// drag a drag.
#[test]
fn winds_sum_by_rate_and_scale_never_accelerates_drag() {
    let field = wind([4.0, 0.0, 0.0], 1.0).sum(&wind([0.0, 0.0, 2.0], 3.0)).unwrap();
    let sample = field.sample_force([0.0; 3]);
    assert!((sample.drag - 4.0).abs() < 1e-6);
    assert!(close(sample.drag_wind, [4.0, 0.0, 6.0], 1e-6));
    let reversed = wind([4.0, 0.0, 0.0], 2.0).scaled(-1.5).unwrap().sample_force([0.0; 3]);
    assert!((reversed.drag - 3.0).abs() < 1e-6);
    assert!(close(reversed.drag_wind, [-12.0, 0.0, 0.0], 1e-6));
    let mask = FieldValue::uniform([1.0; 3]).unwrap();
    assert!(wind([1.0, 0.0, 0.0], 1.0).multiply(&mask).is_err());
    assert!(mask.multiply(&wind([1.0, 0.0, 0.0], 1.0)).is_err());
    assert!(FieldValue::uniform([1.0; 3]).unwrap().air(-1.0).is_err());
}

/// An attractor is a radial field at negative strength. Edge falloff fades to
/// nothing at the radius; Inverse is full inside it and (R/d)^p beyond.
#[test]
fn attractor_pulls_toward_its_centre_with_the_chosen_falloff() {
    let centre = [1.0, 2.0, -1.0];
    let (radius, power, strength) = (2.0f32, 2.0f32, 5.0f32);
    for curve in [FalloffCurve::Edge, FalloffCurve::Inverse] {
        let attractor = FieldValue::radial_curve(centre, radius, power, curve).unwrap().scaled(-strength).unwrap();
        for distance in [0.5f32, 1.0, 1.9, 2.5, 6.0] {
            let at = [centre[0] + distance * 0.6, centre[1], centre[2] + distance * 0.8];
            let pull = attractor.sample(at);
            let expected = strength
                * match curve {
                    FalloffCurve::Edge if distance >= radius => 0.0,
                    FalloffCurve::Edge => (1.0 - distance / radius).powf(power),
                    FalloffCurve::Inverse => (radius / distance.max(radius)).powf(power),
                };
            assert!(close(pull, [-0.6 * expected, 0.0, -0.8 * expected], 1e-5), "{curve:?} {distance}: {pull:?}");
        }
    }
    let inverse = FieldValue::radial_curve(centre, radius, power, FalloffCurve::Inverse).unwrap();
    // Inverse square: twice as far out, a quarter of the pull.
    let near = inverse.sample([centre[0] + 3.0, centre[1], centre[2]])[0];
    let far = inverse.sample([centre[0] + 6.0, centre[1], centre[2]])[0];
    assert!((near / far - 4.0).abs() < 1e-4);
    let vortex = FieldValue::vortex_curve([0.0; 3], [0.0, 1.0, 0.0], 1.0, 1.0, FalloffCurve::Inverse).unwrap();
    assert!(close(vortex.sample([4.0, 0.0, 0.0]), [0.0, 0.0, -0.25], 1e-6));
}

#[test]
fn turbulence_is_a_leaf_that_can_become_wind() {
    let gust = FieldValue::turbulence(1.5, [0.2, 0.0, 0.0], 9).unwrap().scaled(2.0).unwrap().air(1.0).unwrap();
    let leaf = TurbulenceField::new(1.5, [0.2, 0.0, 0.0], 9).unwrap();
    let at = [0.3, 0.7, -0.2];
    let sample = gust.sample_force(at);
    assert!(close(sample.drag_wind, leaf.sample(at).map(|c| 2.0 * c), 1e-6));
    let mut leaves = 0;
    gust.for_each_leaf(|leaf, scale| {
        assert!(matches!(leaf, FieldLeaf::Turbulence(_)));
        assert_eq!(scale, 2.0);
        leaves += 1;
    });
    assert_eq!(leaves, 1);
}
