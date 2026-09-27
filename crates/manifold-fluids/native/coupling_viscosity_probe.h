#pragma once
#include <stdint.h>

struct ManifoldViscousBoundaryProbe {
    double max_linear_balance_error;
    double max_angular_balance_error;
    double max_linear_absolute_error;
    double max_angular_absolute_error;
    double max_rigid_velocity_error;
    double max_rigid_impulse;
    double max_density_scaling_error;
    double max_passive_energy_ratio;
    double low_viscosity_energy_ratio;
    double high_viscosity_energy_ratio;
    uint32_t cases;
};

void run_viscous_boundary_probe(ManifoldViscousBoundaryProbe &result);
