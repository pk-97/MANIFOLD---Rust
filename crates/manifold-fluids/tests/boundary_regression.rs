//! A small ordinary-speed body of water must not evaporate at a closed wall.
use manifold_fluids::{Bounds, Config, FluidWorld, SurfaceOptions};
use manifold_foundation::Seconds;

#[test]
fn coarse_grid_water_survives_closed_boundary_settling() {
    // Eight usable cells plus the production 1.5-cell margin on each side.
    let mut world = FluidWorld::new(Config {
        cells: [11; 3],
        cell_size: 0.5,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    world
        .set_surface_options(SurfaceOptions {
            particle_scale: 2.2,
            smoothing: 0.35,
            smoothing_iterations: 2,
        })
        .unwrap();
    world.set_gravity([0.0, -9.81, 0.0]).unwrap();
    // Authored [0, .8, 1]..[1, 1.8, 2], touching the left wall.
    world
        .add_fluid_box(
            Bounds {
                min: [0.75, 1.55, 1.75],
                max: [1.75, 2.55, 2.75],
            },
            [0.0; 3],
        )
        .unwrap();
    for tick in 1..=180 {
        let stats = world.step(Seconds(1.0 / 60.0)).unwrap();
        assert_eq!(stats.particles, 64, "particle loss at tick {tick}");
        assert!(stats.triangles > 0, "surface vanished at tick {tick}");
    }
    let mut surface = Vec::new();
    world.surface(&mut surface).unwrap();
    assert!(!surface.is_empty());
    assert!(surface.iter().all(|vertex| vertex
        .position
        .iter()
        .chain(&vertex.normal)
        .all(|value| value.is_finite())));
}
