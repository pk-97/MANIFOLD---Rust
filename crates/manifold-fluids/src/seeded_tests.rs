use super::*;

fn impact_world(seed: u64) -> FluidWorld {
    let mut world = FluidWorld::new_seeded(
        Config {
            cells: [16; 3],
            cell_size: 0.25,
            surface_subdivisions: 1,
            apic: false,
        },
        seed,
    )
    .unwrap();
    world
        .set_whitewater_options(WhitewaterOptions {
            enabled: true,
            max_particles: 256,
            wavecrest_rate: 1_000.0,
            turbulence_rate: 1_000.0,
            min_energy: 0.0,
            max_energy: 60.0,
        })
        .unwrap();
    world.set_gravity([0.0, -9.81, 0.0]).unwrap();
    // Exercise geometry sampling as well as marker and diffuse particle RNGs.
    let points = [0.4, 3.6].into_iter().flat_map(|x| {
        [0.4, 1.4]
            .into_iter()
            .flat_map(move |y| [0.4, 3.6].into_iter().map(move |z| [x, y, z]))
    });
    let mesh = manifold_physics::cook_hull_mesh(&points.collect::<Vec<_>>()).unwrap();
    world
        .add_fluid_mesh(
            &mesh,
            manifold_physics::BodyPose {
                position: [0.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            [0.0; 3],
        )
        .unwrap();
    world
        .set_emitter(
            Bounds {
                min: [1.5, 2.0, 1.5],
                max: [2.5, 2.5, 2.5],
            },
            [0.0, -8.0, 0.0],
            true,
        )
        .unwrap();
    world
}

#[test]
fn seeded_native_mesh_and_whitewater_replay_survives_interleaved_worlds() {
    const SEED: u64 = 0x3141_5926_5358_9793;
    const TICKS: usize = 36;
    let mut baseline = impact_world(SEED);
    for _ in 0..TICKS {
        baseline.step(Seconds(1.0 / 60.0)).unwrap();
    }
    let mut expected = Vec::new();
    baseline.whitewater(&mut expected).unwrap();
    assert!(!expected.is_empty(), "impact fixture must emit whitewater");
    assert!(expected.len() <= 256);
    let mut expected_surface = Vec::new();
    baseline.surface(&mut expected_surface).unwrap();
    assert!(!expected_surface.is_empty());

    let mut unrelated = impact_world(SEED ^ (1 << 40));
    let mut replay = impact_world(SEED);
    for _ in 0..TICKS {
        unrelated.step(Seconds(1.0 / 60.0)).unwrap();
        replay.step(Seconds(1.0 / 60.0)).unwrap();
    }
    let mut actual = Vec::new();
    replay.whitewater(&mut actual).unwrap();
    assert_eq!(actual, expected, "another world must not advance this RNG");
    unrelated.whitewater(&mut actual).unwrap();
    assert_ne!(actual, expected, "upper seed bits must affect emission");

    let mut actual_surface = Vec::new();
    replay.surface(&mut actual_surface).unwrap();
    assert_eq!(actual_surface.len(), expected_surface.len());
    for (actual, expected) in actual_surface.iter().zip(&expected_surface) {
        for (a, b) in actual.position.into_iter().zip(expected.position) {
            assert!((a - b).abs() < 1e-5, "surface replay: {a} != {b}");
        }
    }
}
