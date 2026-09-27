//! Pressure-stage feasibility gates; no Box3D world, mesh reconstruction,
//! viscosity, advection or production two-way scheduling is exercised here.

#[repr(C)]
#[derive(Debug, Default)]
struct Probe {
    impulse: [f64; 3],
    moment: [f64; 3],
    max_fluid_speed: f64,
    pressure_residual: f64,
    added_mass: f64,
    first_body_energy_ratio: f64,
    first_pressure_residual: f64,
    max_body_energy_ratio: f64,
}

unsafe extern "C" {
    fn manifold_fluids_coupling_pressure_probe(
        resolution: u32,
        dt: f64,
        density: f64,
        exchanges: u32,
        body_density_ratio: f64,
        result: *mut Probe,
    ) -> i32;
}

fn probe(resolution: u32, dt: f64, density: f64, exchanges: u32, ratio: f64) -> Probe {
    let mut result = Probe::default();
    // Native entry uses the same global lock and error handling as FluidWorld.
    let status = unsafe {
        manifold_fluids_coupling_pressure_probe(
            resolution,
            dt,
            density,
            exchanges,
            ratio,
            &mut result,
        )
    };
    super::super::native_result(status, "pressure-coupling feasibility").unwrap();
    println!(
        "r={resolution} dt={dt} rho={density} exchanges={exchanges} ratio={ratio}: {result:?}"
    );
    result
}

#[test]
fn coupling_hydrostatic_force_and_offcentre_torque() {
    // Predetermined acceptance: 5% at dx=.25m, 2.5% at dx=.125m;
    // off-axis force/torque < .01%, projected rest speed < 1e-5 m/s.
    for (resolution, tolerance) in [(4, 0.05), (8, 0.025)] {
        let dt = 1.0 / 60.0;
        let result = probe(resolution, dt, 1000.0, 0, 1.0);
        let expected = 1000.0 * 9.81 * dt; // rho * g * 1m^3 * dt
        assert!(
            (result.impulse[1] / expected - 1.0).abs() < tolerance,
            "{result:?}"
        );
        assert!(
            (result.moment[2] / (0.25 * expected) - 1.0).abs() < tolerance,
            "{result:?}"
        );
        for other in [
            result.impulse[0],
            result.impulse[2],
            result.moment[0],
            result.moment[1],
        ] {
            assert!(other.abs() < expected * 1e-4, "{result:?}");
        }
        assert!(result.max_fluid_speed < 1e-5, "{result:?}");
        assert!(result.pressure_residual < 1e-6, "{result:?}");
    }
}

#[test]
fn coupling_pressure_units_scale_with_density_and_interval() {
    // Force must remain rho*g*volume when dt halves or density doubles.
    for (dt, density) in [(1.0 / 60.0, 500.0), (1.0 / 120.0, 1000.0)] {
        let result = probe(4, dt, density, 0, 1.0);
        let relative = result.impulse[1] / (density * 9.81 * dt);
        assert!((relative - 1.0).abs() < 0.005, "{result:?}");
    }
}

#[test]
fn coupling_partitioned_light_body_rejects_energy_growth() {
    // Candidate only: project fluid against prescribed solid velocity, then
    // apply the measured reaction to that body's mass. A body alone exceeding
    // the initial TOTAL energy disproves stability even before counting fluid KE.
    // Geometry is frozen to isolate exchange; this is not a floating-body test.
    let coarse = probe(4, 1.0 / 60.0, 1000.0, 8, 0.1);
    let smaller_step = probe(4, 1.0 / 120.0, 1000.0, 8, 0.1);
    let fine = probe(8, 1.0 / 120.0, 1000.0, 8, 0.1);
    for result in [&coarse, &smaller_step, &fine] {
        assert!(
            result.added_mass > 0.0,
            "reaction must oppose acceleration: {result:?}"
        );
        assert!(result.first_pressure_residual < 1e-6, "{result:?}");
        assert!(
            result.first_body_energy_ratio > 1.01,
            "candidate no longer demonstrates its known instability; re-evaluate feasibility: {result:?}"
        );
        assert!(
            result.max_body_energy_ratio > result.first_body_energy_ratio,
            "{result:?}"
        );
    }
    assert!((coarse.added_mass / smaller_step.added_mass - 1.0).abs() < 0.005);
    // A heavy body control distinguishes the light-body instability from a
    // universally incorrect reaction sign. It is not a general stability proof.
    let heavy = probe(4, 1.0 / 60.0, 1000.0, 8, 10.0);
    assert!(heavy.max_body_energy_ratio <= 1.01, "{heavy:?}");
}
