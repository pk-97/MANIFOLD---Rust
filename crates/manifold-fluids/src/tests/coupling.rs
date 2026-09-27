//! Native pressure, boundary attribution and prescribed viscous stress gates.
//! No Box3D world, advection or production two-way scheduling is exercised here.

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
    max_total_energy_ratio: f64,
    max_coupling_relative_mismatch: f64,
    max_volume_residual: f64,
}

#[repr(C)]
#[derive(Debug, Default)]
struct BoundaryProbe {
    max_velocity_error: f64,
    max_transpose_error: f64,
    blended_faces: u32,
    extrapolated_faces: u32,
}

#[repr(C)]
#[derive(Debug, Default)]
struct ViscousProbe {
    max_linear_balance_error: f64,
    max_angular_balance_error: f64,
    max_linear_absolute_error: f64,
    max_angular_absolute_error: f64,
    max_rigid_velocity_error: f64,
    max_rigid_impulse: f64,
    max_density_scaling_error: f64,
    max_passive_energy_ratio: f64,
    low_viscosity_energy_ratio: f64,
    high_viscosity_energy_ratio: f64,
    cases: u32,
}

fn assert_probe_finite(result: &Probe) {
    assert!(
        result.impulse.iter().all(|value| value.is_finite()),
        "{result:?}"
    );
    assert!(
        result.moment.iter().all(|value| value.is_finite()),
        "{result:?}"
    );
    assert!(result.max_fluid_speed.is_finite(), "{result:?}");
    assert!(result.pressure_residual.is_finite(), "{result:?}");
    assert!(result.added_mass.is_finite(), "{result:?}");
    assert!(result.first_body_energy_ratio.is_finite(), "{result:?}");
    assert!(result.first_pressure_residual.is_finite(), "{result:?}");
    assert!(result.max_body_energy_ratio.is_finite(), "{result:?}");
    assert!(result.max_total_energy_ratio.is_finite(), "{result:?}");
    assert!(
        result.max_coupling_relative_mismatch.is_finite(),
        "{result:?}"
    );
    assert!(result.max_volume_residual.is_finite(), "{result:?}");
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
    fn manifold_fluids_coupling_pressure_probe_mode(
        resolution: u32,
        dt: f64,
        density: f64,
        exchanges: u32,
        body_density_ratio: f64,
        mode: u32,
        result: *mut Probe,
    ) -> i32;
    fn manifold_fluids_coupling_operator_probe() -> i32;
    fn manifold_fluids_coupling_closed_pocket_probe() -> i32;
    fn manifold_fluids_coupling_boundary_probe(result: *mut BoundaryProbe) -> i32;
    fn manifold_fluids_coupling_viscosity_probe(result: *mut ViscousProbe) -> i32;
}

#[test]
fn coupling_viscous_boundary_balances_momentum_and_dissipates_energy() {
    let mut result = ViscousProbe::default();
    let status = unsafe { manifold_fluids_coupling_viscosity_probe(&mut result) };
    println!("{result:?}");
    super::super::native_result(status, "viscous boundary reaction").unwrap();
    assert_eq!(result.cases, 17);
    assert!(result.max_linear_absolute_error.is_finite());
    assert!(result.max_angular_absolute_error.is_finite());
    assert!(result.max_linear_balance_error.is_finite() && result.max_linear_balance_error < 1e-4);
    assert!(
        result.max_angular_balance_error.is_finite() && result.max_angular_balance_error < 1e-4
    );
    assert!(result.max_rigid_velocity_error.is_finite() && result.max_rigid_velocity_error < 2e-5);
    assert!(result.max_rigid_impulse.is_finite() && result.max_rigid_impulse < 1e-4);
    assert!(
        result.max_density_scaling_error.is_finite() && result.max_density_scaling_error < 1e-6
    );
    assert!(
        result.max_passive_energy_ratio.is_finite() && result.max_passive_energy_ratio <= 1.00001
    );
    assert!(result.high_viscosity_energy_ratio < result.low_viscosity_energy_ratio);
}

#[test]
fn coupling_mesh_boundary_matches_native_interpolation_and_pressure_work() {
    let mut result = BoundaryProbe::default();
    let status = unsafe { manifold_fluids_coupling_boundary_probe(&mut result) };
    super::super::native_result(status, "rigid boundary attribution").unwrap();
    println!("{result:?}");
    assert!(result.max_velocity_error.is_finite() && result.max_velocity_error < 2e-5);
    assert!(result.max_transpose_error.is_finite() && result.max_transpose_error < 3e-5);
    assert!(result.blended_faces > 0 && result.extrapolated_faces > 0);
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
    assert_probe_finite(&result);
    println!(
        "r={resolution} dt={dt} rho={density} exchanges={exchanges} ratio={ratio}: {result:?}"
    );
    result
}

fn probe_mode(
    resolution: u32,
    dt: f64,
    density: f64,
    exchanges: u32,
    ratio: f64,
    mode: u32,
) -> Probe {
    let mut result = Probe::default();
    let status = unsafe {
        manifold_fluids_coupling_pressure_probe_mode(
            resolution,
            dt,
            density,
            exchanges,
            ratio,
            mode,
            &mut result,
        )
    };
    super::super::native_result(status, "mass-aware pressure-coupling feasibility").unwrap();
    assert_probe_finite(&result);
    println!(
        "mode={mode} r={resolution} dt={dt:.6} ratio={ratio}: first_body_energy={:.6}, max_total_energy={:.6}, volume_residual={:.3e}, reaction_mismatch={:.3e}",
        result.first_body_energy_ratio,
        result.max_total_energy_ratio,
        result.max_volume_residual,
        result.max_coupling_relative_mismatch
    );
    result
}

#[test]
fn coupling_operator_probe_matches_dense_reference() {
    let status = unsafe { manifold_fluids_coupling_operator_probe() };
    super::super::native_result(status, "pressure-coupling operator algebra").unwrap();
}

#[test]
fn coupling_closed_pocket_resolves_body_constraint_and_rejects_fixed_compression() {
    let status = unsafe { manifold_fluids_coupling_closed_pocket_probe() };
    super::super::native_result(status, "closed-pocket pressure constraint").unwrap();
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

#[test]
fn coupling_mass_aware_exchange_does_not_add_energy() {
    for mode in [1, 2] {
        for ratio in [0.1, 1.0, 10.0] {
            let mut coarse: Option<Probe> = None;
            for (resolution, dt) in [(4, 1.0 / 60.0), (4, 1.0 / 120.0), (8, 1.0 / 120.0)] {
                let result = probe_mode(resolution, dt, 1000.0, 8, ratio, mode);
                assert!(result.max_total_energy_ratio <= 1.001, "{result:?}");
                assert!(result.max_coupling_relative_mismatch < 1e-4, "{result:?}");
                assert!(result.max_volume_residual < 1e-5, "{result:?}");
                assert!(result.first_pressure_residual < 1e-6, "{result:?}");
                if resolution == 4 {
                    if let Some(reference) = &coarse {
                        // A frozen-geometry projection has the same impulse
                        // when dt halves; pressure itself scales as 1/dt.
                        let (mut error, mut norm) = (0.0, 0.0);
                        for (actual, expected) in result
                            .impulse
                            .iter()
                            .chain(&result.moment)
                            .zip(reference.impulse.iter().chain(&reference.moment))
                        {
                            error += (actual - expected).powi(2);
                            norm += expected.powi(2);
                        }
                        assert!(error < norm.max(1e-24) * 1e-8, "{result:?}");
                        assert!(
                            (result.first_body_energy_ratio - reference.first_body_energy_ratio)
                                .abs()
                                < 1e-4
                        );
                    } else {
                        coarse = Some(result);
                    }
                }
            }
        }
    }
}

#[test]
fn coupling_fixed_body_preserves_pressure_reaction() {
    for (resolution, tolerance) in [(4, 0.05), (8, 0.025)] {
        let explicit = probe(resolution, 1.0 / 60.0, 1000.0, 0, 1.0);
        let fixed = probe_mode(resolution, 1.0 / 60.0, 1000.0, 0, 1.0, 3);
        for axis in 0..3 {
            assert!(
                (fixed.impulse[axis] - explicit.impulse[axis]).abs() < tolerance,
                "{fixed:?}"
            );
            assert!(
                (fixed.moment[axis] - explicit.moment[axis]).abs() < tolerance,
                "{fixed:?}"
            );
        }
        assert!(fixed.max_coupling_relative_mismatch < 1e-4, "{fixed:?}");
    }
}
