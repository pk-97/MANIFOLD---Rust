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

struct ManifoldViscousFeedbackProbe {
    // Ordered by density ratio {0.1,1}, viscosity {1,10}, dt {1/60,1/120}.
    double impulses[8];
    double energy_ratios[8];
};

void run_viscous_feedback_probe(ManifoldViscousFeedbackProbe &result);

struct ManifoldCoupledViscosityProbe {
    double max_energy_ratio;
    double max_response_error;
    double max_transpose_error;
    double fixed_velocity_error;
    uint32_t cases;
};

void run_coupled_viscosity_probe(ManifoldCoupledViscosityProbe &result);
