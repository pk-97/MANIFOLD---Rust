// MANIFOLD: geometry-only reconstruction of an independently owned frame.
#include "surfaceframe.h"
#include "particlemesher.h"
#include "spatialpointgrid.h"
#include <algorithm>
#include <cmath>

void FluidSurfaceFrame::_prepareIsolatedRadii(double radius, double isolatedScale) {
    if (_isolationRadius == radius && _radiiScale == isolatedScale) {
        return;
    }
    _particleRadii.resize(particles.size());
    // Any overlapping reconstruction spheres retain their full radius. The
    // transition depends on the original radius, never on mesh subdivisions
    // or the selected shrink factor, so tuning cannot reclassify the inputs.
    if (_isolationRadius != radius) {
        _isolationWeights.resize(particles.size());
        SpatialPointGrid grid(isize, jsize, ksize, dx);
        auto references = grid.insert(particles);
        std::vector<GridPointReference> neighbours;
        const double connectedDistance = 2.0 * radius;
        const double searchDistance = 3.0 * radius;
        for (size_t i = 0; i < particles.size(); ++i) {
            if (grid.hasPointWithinSphere(references[i], connectedDistance)) {
                _isolationWeights[i] = 0.0f;
                continue;
            }
            neighbours.clear();
            grid.queryPointReferencesInsideSphere(references[i], searchDistance, neighbours);
            double nearestSquared = searchDistance * searchDistance;
            for (auto neighbour : neighbours) {
                const auto delta = particles[i] - particles[neighbour.id];
                nearestSquared = std::min(nearestSquared, static_cast<double>(vmath::dot(delta, delta)));
            }
            double t = std::clamp((std::sqrt(nearestSquared) - connectedDistance) / radius, 0.0, 1.0);
            _isolationWeights[i] = static_cast<float>(t * t * (3.0 - 2.0 * t));
        }
        _isolationRadius = radius;
    }
    for (size_t i = 0; i < particles.size(); ++i) {
        _particleRadii[i] = static_cast<float>(radius * (1.0 + (isolatedScale - 1.0) * _isolationWeights[i]));
    }
    _radiiScale = isolatedScale;
}

TriangleMesh FluidSurfaceFrame::mesh(int subdivisions, double particleScale,
                                   double smoothing, int iterations, double isolatedScale) {
    TriangleMesh surface;
    if (particles.empty()) {
        return surface;
    }
    ParticleMesherParameters params;
    params.isize = isize;
    params.jsize = jsize;
    params.ksize = ksize;
    params.dx = dx;
    params.subdivisions = subdivisions;
    params.computechunks = chunks;
    params.radius = particleRadius * particleScale;
    params.particles = &particles;
    params.solidSDF = &solid;
    if (isolatedScale < 1.0) {
        _prepareIsolatedRadii(params.radius, isolatedScale);
        params.particleRadii = &_particleRadii;
    }
    ParticleMesher mesher;
    surface = mesher.meshParticles(params);
    surface.removeMinimumTriangleCountPolyhedra(minimumTriangles);
    surface.smooth(smoothing, iterations);
    surface.scale(vmath::vec3(domainScale, domainScale, domainScale));
    surface.translate(domainOffset);
    return surface;
}
