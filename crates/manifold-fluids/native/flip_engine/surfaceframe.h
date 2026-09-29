// MANIFOLD: owned inputs for geometry-only reconstruction of one accepted frame.
#pragma once

#include "meshlevelset.h"
#include "trianglemesh.h"
#include "vmath.h"
#include <vector>

// No solver or MeshObject pointers are retained. The minimal level set owns only
// distance values; capture is explicit, so ordinary playback does not copy it.
struct FluidSurfaceFrame {
    int isize = 0, jsize = 0, ksize = 0;
    int chunks = 1;
    int minimumTriangles = 0;
    double dx = 0.0;
    double particleRadius = 0.0;
    double domainScale = 1.0;
    vmath::vec3 domainOffset;
    std::vector<vmath::vec3> particles;
    MeshLevelSet solid;

    TriangleMesh mesh(int subdivisions, double particleScale,
                      double smoothing, int iterations, double isolatedScale = 1.0);

private:
    std::vector<float> _particleRadii;
    std::vector<float> _isolationWeights;
    double _isolationRadius = 0.0;
    double _radiiScale = 0.0;
    void _prepareIsolatedRadii(double radius, double isolatedScale);
};
