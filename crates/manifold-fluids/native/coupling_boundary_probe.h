#pragma once
#include <stdint.h>

struct ManifoldRigidBoundaryProbe {
    double max_velocity_error;
    double max_transpose_error;
    uint32_t blended_faces;
    uint32_t extrapolated_faces;
};

void run_rigid_boundary_probe(ManifoldRigidBoundaryProbe &result);
