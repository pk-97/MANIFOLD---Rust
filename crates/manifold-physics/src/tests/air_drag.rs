//! A rigid body in wind relaxes to the wind's velocity at the drag rate.

use super::*;

#[test]
fn rigid_body_in_wind_relaxes_to_the_wind_velocity() {
    let dt = Seconds(1.0 / 60.0);
    let wind = FieldValue::uniform([4.0, 0.0, 0.0]).unwrap().air(2.0).unwrap();
    let input = FieldInput { field: &wind, acceleration: 1.0, delta_velocity: 0.0 };
    let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = world.add_hull(&cube(0.5), BodyConfig { position: [0.0, 10.0, 0.0], ..BodyConfig::default() }).unwrap();
    world.set_velocity(body, [0.0, 0.0, 3.0], [0.0; 3]).unwrap();
    for _ in 0..60 {
        world.apply_fields(&[body], &[input], dt).unwrap();
        world.step(dt, 4).unwrap();
    }
    let velocity = world.linear_velocity(body).unwrap();
    let decay = (-2.0f32).exp();
    let expected = [4.0 * (1.0 - decay), 0.0, 3.0 * decay];
    for axis in 0..3 {
        assert!((velocity[axis] - expected[axis]).abs() < 0.02, "{velocity:?} vs {expected:?}");
    }

    // The same wind with the body's Air Drag at 0 leaves it coasting.
    let still = wind.with_air_drag(0.0).unwrap();
    let input = FieldInput { field: &still, acceleration: 1.0, delta_velocity: 0.0 };
    world.set_velocity(body, [0.0, 0.0, 3.0], [0.0; 3]).unwrap();
    for _ in 0..30 {
        world.apply_fields(&[body], &[input], dt).unwrap();
        world.step(dt, 4).unwrap();
    }
    let coasting = world.linear_velocity(body).unwrap();
    assert!(coasting[0].abs() < 1e-4 && (coasting[2] - 3.0).abs() < 1e-3, "{coasting:?}");
}
