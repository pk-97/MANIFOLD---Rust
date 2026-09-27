#pragma once

#include <stdint.h>

// Bounded native numerical fixture. Not a FluidWorld or production coupling API.
struct ManifoldFluidsCouplingProbe {
    double impulse[3];
    double moment[3];
    double max_fluid_speed;
    double pressure_residual;
    double added_mass;
    double first_body_energy_ratio;
    double first_pressure_residual;
    double max_body_energy_ratio;
};

void run_coupling_pressure_probe(uint32_t cells_per_meter, double dt, double density,
                                uint32_t exchanges, double body_density_ratio,
                                ManifoldFluidsCouplingProbe &result);
