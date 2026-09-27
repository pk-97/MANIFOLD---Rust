#pragma once

struct ManifoldRigidViscosityProbe {
    double max_solution_error;
    double max_response_error;
    double max_symmetry_error;
    double max_diagonal_error;
    double max_energy_ratio;
};

void run_rigid_viscosity_operator_probe(ManifoldRigidViscosityProbe &result);
